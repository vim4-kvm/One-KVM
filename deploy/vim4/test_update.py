import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader('onekvm_update', str(Path(__file__).with_name('vim4-onekvm-update')))
spec = importlib.util.spec_from_loader(loader.name, loader)
helper = importlib.util.module_from_spec(spec)
loader.exec_module(helper)


class UpdateTests(unittest.TestCase):
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
