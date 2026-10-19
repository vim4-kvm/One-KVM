import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader('onekvm_update', str(Path(__file__).with_name('vim4-onekvm-update')))
spec = importlib.util.spec_from_loader(loader.name, loader)
helper = importlib.util.module_from_spec(spec)
loader.exec_module(helper)


class UpdateTests(unittest.TestCase):
    def test_install_waits_for_frontend_and_database_locks(self):
        for name in ('dpkg frontend lock', 'dpkg database lock'):
            error = subprocess.CalledProcessError(2, ['dpkg'],
                output=f'dpkg: error: {name} was locked by another process with pid 123\n')
            with self.subTest(name=name), patch.object(helper, 'run', side_effect=[error, 'installed']) as run, patch.object(helper.time, 'sleep') as sleep:
                self.assertEqual(helper.install_package(Path('/update.deb')), 'installed')
                self.assertEqual(run.call_count, 2)
                sleep.assert_called_once()

    def test_install_lock_wait_is_bounded_and_reports_owner(self):
        error = subprocess.CalledProcessError(2, ['dpkg'],
            output='dpkg: error: dpkg frontend lock was locked by another process with pid 123\n')
        with patch.object(helper, 'run', side_effect=error) as run, patch.object(helper.time, 'monotonic', side_effect=[0, 119, 120]), patch.object(helper.time, 'sleep') as sleep:
            with self.assertRaisesRegex(ValueError, 'remained busy for 120 seconds[\\s\\S]*pid 123'):
                helper.install_package(Path('/update.deb'))
            self.assertEqual(run.call_count, 2)
            sleep.assert_called_once_with(1)

    def test_install_never_retries_other_failures(self):
        for code, output in [(1, 'post-installation script returned error'),
                             (2, 'dpkg: error: unable to access archive'),
                             (2, 'Unpacking package\ndpkg: error: dpkg database lock was locked by another process')]:
            error = subprocess.CalledProcessError(code, ['dpkg'], output=output)
            with self.subTest(output=output), patch.object(helper, 'run', side_effect=error) as run, patch.object(helper.time, 'sleep') as sleep:
                with self.assertRaises(subprocess.CalledProcessError) as caught:
                    helper.install_package(Path('/update.deb'))
                self.assertIs(caught.exception, error)
                run.assert_called_once()
                sleep.assert_not_called()

    def test_platform_changes_and_removal_are_rejected(self):
        helper.validate_plan('Inst vim4-onekvm [old] (new local)\nConf vim4-onekvm')
        for plan in ['Inst vim4-kvm-board (new local)', 'Remv vim4-onekvm [old]', 'Inst libc6 (new repo)']:
            with self.assertRaises(ValueError):
                helper.validate_plan(plan)

    def test_package_snapshot_rejects_links_and_paths_outside_downloads(self):
        with tempfile.TemporaryDirectory() as temp:
            downloads = Path(temp).resolve() / 'updates'
            directory = downloads / 'download-test'
            directory.mkdir(parents=True)
            package = directory / 'update.deb'
            package.write_bytes(b'package')
            with patch.object(helper, 'DOWNLOADS', downloads):
                with helper.open_download(package) as incoming:
                    self.assertEqual(incoming.read(), b'package')
                with self.assertRaises(ValueError):
                    helper.open_download(Path(temp) / 'update.deb')
                saved = directory / 'original'
                package.rename(saved)
                package.symlink_to(saved)
                with self.assertRaises(OSError):
                    helper.open_download(package)
                package.unlink()
                package.hardlink_to(saved)
                with self.assertRaises(ValueError):
                    helper.open_download(package)

    def test_checksum_failure_does_not_run_package_commands(self):
        with tempfile.TemporaryDirectory() as temp:
            downloads = Path(temp).resolve() / 'updates'
            directory = downloads / 'download-test'
            directory.mkdir(parents=True)
            package = directory / 'update.deb'
            package.write_bytes(b'package')
            with patch.object(helper, 'DOWNLOADS', downloads), patch.object(helper, 'STAGING', Path(temp) / 'root'), patch.object(helper, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
                    helper.install(str(package), '0' * 64, '2.5.1+vim4.20261014.8')
                run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
