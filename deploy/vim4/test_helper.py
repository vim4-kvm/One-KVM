import importlib.machinery
import importlib.util
import os
import copy
import io
import json
from pathlib import Path
import tempfile
import unittest
import sys
from contextlib import nullcontext, redirect_stdout
from unittest.mock import patch

board_dir = Path(os.environ.get('VIM4_BOARD_DIR', Path(__file__).resolve().parents[3] / 'board'))
board_spec = importlib.util.spec_from_file_location('vim4media', board_dir / 'vim4media.py')
vim4media = importlib.util.module_from_spec(board_spec)
sys.modules['vim4media'] = vim4media
board_spec.loader.exec_module(vim4media)

loader = importlib.machinery.SourceFileLoader('helper', str(Path(__file__).with_name('vim4-onekvm')))
spec = importlib.util.spec_from_loader(loader.name, loader)
helper = importlib.util.module_from_spec(spec)
loader.exec_module(helper)


class MediaTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory())).resolve()
        self.media = self.root / 'msd'
        (self.media / 'images').mkdir(parents=True)
        self.enterContext(patch.object(helper, 'MEDIA', self.media))
        self.enterContext(patch.object(vim4media, 'IMAGE_ROOTS', {'onekvm': self.media}))
        self.enterContext(patch.object(vim4media, 'media_locks', side_effect=lambda wait=False: nullcontext()))
        self.enterContext(patch.object(helper, 'PROFILE_DIR', self.root / 'profile'))
        self.image = self.media / 'images' / 'test.iso'
        self.image.write_bytes(b'0' * 512)

    def test_descriptor_survives_path_replacement(self):
        fd = helper.open_image(str(self.image), False)
        try:
            self.image.rename(self.image.with_suffix('.saved'))
            self.image.write_bytes(b'1' * 512)
            self.assertEqual(os.read(fd, 1), b'0')
        finally:
            os.close(fd)

    def test_rejects_links_and_outside_images(self):
        with self.assertRaises(ValueError):
            helper.open_image('/etc/shadow', False)
        link = self.image.with_name('link.iso')
        link.symlink_to(self.image)
        with self.assertRaises(OSError):
            helper.open_image(str(link), False)
        link.unlink()
        os.link(self.image, link)
        with self.assertRaises(ValueError):
            helper.open_image(str(link), False)
        link.unlink()
        directory = self.media / 'images'
        directory.rename(self.media / 'real')
        directory.symlink_to(self.media / 'real')
        with self.assertRaises(OSError):
            helper.open_image(str(self.image), False)

    def test_mount_and_eject_exchange_ncm_without_removing_hid(self):
        gadget = self.root / 'gadget'
        config = gadget / 'configs/c.1'
        config.mkdir(parents=True)
        lun = gadget / 'functions/mass_storage.disk0/lun.0'
        lun.mkdir(parents=True)
        for key in ('file', 'cdrom', 'ro', 'inquiry_string'):
            (lun / key).write_text('' if key == 'file' else '0')
        (gadget / 'UDC').write_text('controller')
        for name in ('hid.keyboard', 'hid.absolute', 'hid.relative', 'ncm.usb0'):
            (config / name).symlink_to(gadget / 'functions' / name)
        write_text = Path.write_text
        def kernel_write(path, value, *args, **kwargs):
            if path == lun / 'file' and value.startswith('/proc/self/fd/'):
                value = os.readlink(value)
            return write_text(path, value, *args, **kwargs)
        with patch.object(helper, 'GADGET', gadget), patch.object(helper, 'LUN', lun), \
                patch.object(helper.time, 'sleep'), patch.object(helper.os, 'sync'), \
                patch.object(Path, 'write_text', kernel_write), \
                patch.object(helper, 'saved_profile', return_value=None):
            output = io.StringIO()
            with redirect_stdout(output):
                helper.main(['media', str(self.image), 'true', 'false'])
            self.assertEqual(json.loads(output.getvalue())['file'], str(self.image))
            self.assertEqual((lun / 'ro').read_text(), '1')
            self.assertTrue((config / 'mass_storage.disk0').is_symlink())
            self.assertFalse((config / 'ncm.usb0').is_symlink())
            helper.main(['media', '', 'false', 'true', (lun / 'file').read_text()])
            self.assertTrue((config / 'ncm.usb0').is_symlink())
            self.assertFalse((config / 'mass_storage.disk0').is_symlink())
            self.assertEqual((gadget / 'UDC').read_text(), 'controller')
            for name in ('hid.keyboard', 'hid.absolute', 'hid.relative'):
                self.assertTrue((config / name).is_symlink())

    def test_foreign_media_and_racing_replacement_are_refused_before_writes(self):
        gadget = self.root / 'gadget'
        lun = gadget / 'functions/mass_storage.disk0/lun.0'
        lun.mkdir(parents=True)
        (gadget / 'UDC').write_text('controller')
        for key, value in {'file': str(self.image), 'ro': '0', 'cdrom': '0',
                           'inquiry_string': 'disk'}.items():
            (lun / key).write_text(value)
        before = {path: path.read_bytes() for path in lun.iterdir()}
        with patch.object(helper, 'GADGET', gadget), patch.object(helper, 'LUN', lun):
            for name, expected in [(str(self.image), ''), ('', ''), ('', '/previous.img')]:
                with self.assertRaisesRegex(ValueError, 'MSD_EXTERNAL_MEDIA'):
                    helper.configure_media(name, False, False, expected)
                self.assertEqual((gadget / 'UDC').read_text(), 'controller')
                self.assertEqual({path: path.read_bytes() for path in lun.iterdir()}, before)

    def test_rejects_unrecognized_actions_before_mutation(self):
        for args in (['reboot'], ['media', '', 'true', 'yes'], ['media']):
            with self.assertRaises(ValueError):
                helper.main(args)

    def test_foreign_policy_and_data_edit_lock(self):
        import fcntl
        self.enterContext(patch.object(vim4media, 'DATA_DIR', self.media / 'images'))
        with patch.object(helper, '_configure_media') as configure:
            helper.configure_media('pikvm/test.iso', False, False)
            configure.assert_called_with('pikvm/test.iso', True, True, '')
            helper.configure_media('jetkvm/test.img', False, False)
            configure.assert_called_with('jetkvm/test.img', False, True, '')
            helper.configure_media('data/test.img', True, False)
            configure.assert_called_with('data/test.img', False, False, '')
        disk = self.media / 'images/test.img'
        disk.write_bytes(b'd' * 512)
        with disk.open('r+b') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(ValueError, '正在管理面板中编辑'):
                helper.configure_media('data/test.img', False, False)

    def test_locks_are_held_when_opening_media(self):
        from contextlib import contextmanager
        held = []

        @contextmanager
        def locks(wait=False):
            self.assertTrue(wait, 'application helpers wait for the shared media lock')
            held.append(True)
            try:
                yield
            finally:
                held.pop()

        def open_locked(*args, **kwargs):
            self.assertEqual(held, [True])
            raise ValueError('stop before touching LUN')

        vim4media.media_locks.side_effect = locks
        with patch.object(vim4media, 'open_image', side_effect=open_locked):
            with self.assertRaisesRegex(ValueError, 'stop before touching LUN'):
                helper.configure_media('data/test.img', False, False)
        self.assertEqual(held, [])


class UsbProfileTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.gadget = self.root / 'gadget'
        self.config = self.gadget / 'configs/c.1'
        self.config.mkdir(parents=True)
        self.lun = self.gadget / 'functions/mass_storage.disk0/lun.0'
        self.lun.mkdir(parents=True)
        for key in ('file', 'ro', 'cdrom', 'inquiry_string'):
            (self.lun / key).write_text('')
        (self.gadget / 'UDC').write_text('controller')
        self.enterContext(patch.object(helper, 'GADGET', self.gadget))
        self.enterContext(patch.object(helper, 'LUN', self.lun))
        self.enterContext(patch.object(helper, 'PROFILE_DIR', self.root / 'profile'))
        self.enterContext(patch.object(helper.time, 'sleep'))
        self.network = self.enterContext(patch.object(helper, 'configure_network'))
        self.enterContext(patch.object(helper, 'settle_usb'))
        descriptors = dict(zip(helper.DESCRIPTORS, ['0x1d6b', '0x0104', 'Board', 'KVM', 'serial']))
        for key, value in descriptors.items():
            path = self.gadget / key
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(value)
        for name in ('hid.keyboard', 'hid.mouse_abs', 'hid.mouse_rel', 'ncm.usb0', 'uac2.usb0'):
            (self.gadget / 'functions' / name).mkdir()
            if name != 'uac2.usb0':
                (self.config / name).symlink_to(self.gadget / 'functions' / name)
        self.baseline = helper.snapshot_usb()
        self.profile = {'hid': {'keyboard': True, 'mouse_absolute': True, 'mouse_relative': True, 'consumer': True},
                        'descriptor': descriptors, 'network': '', 'uac': False, 'serial': False,
                        'inquiry': {'flash': 'One-KVM Disk', 'cdrom': 'One-KVM CD-ROM'}}

    def test_switch_profiles_preserves_instances_and_restores_board(self):
        self.profile['descriptor']['strings/0x409/product'] = 'Custom KVM'
        helper.configure_profile(self.profile)
        self.assertTrue((self.config / 'hid.consumer').is_symlink())
        self.assertFalse((self.config / 'ncm.usb0').exists())
        self.profile['hid']['keyboard'] = False
        self.profile['uac'] = True
        helper.configure_profile(self.profile)
        self.assertFalse((self.config / 'hid.keyboard').exists())
        self.assertTrue((self.gadget / 'functions/hid.keyboard').is_dir())
        self.assertTrue((self.config / 'uac2.usb0').is_symlink())
        helper.restore_profile()
        self.assertEqual(helper.snapshot_usb(), self.baseline)

    def test_endpoint_limit_checks_media_before_disconnect(self):
        self.profile['network'] = 'ncm'
        with self.assertRaisesRegex(ValueError, 'endpoint limit'):
            helper.configure_profile(self.profile)
        self.assertEqual(helper.snapshot_usb(), self.baseline)
        self.profile['hid']['consumer'] = False
        (self.lun / 'file').write_text('/mounted.img')
        with self.assertRaisesRegex(ValueError, 'endpoint limit'):
            helper.configure_profile(self.profile)
        self.assertEqual((self.gadget / 'UDC').read_text(), 'controller')

    def test_clearing_custom_serial_restores_board_identity(self):
        self.profile['descriptor']['strings/0x409/serialnumber'] = 'custom-serial'
        helper.configure_profile(self.profile)
        self.profile['descriptor']['strings/0x409/serialnumber'] = None
        helper.configure_profile(self.profile)
        self.assertEqual((self.gadget / 'strings/0x409/serialnumber').read_text(), 'serial')

    def test_serial_costs_two_endpoints_and_is_removed_on_restore(self):
        self.profile['serial'] = True
        with self.assertRaisesRegex(ValueError, 'endpoint limit'):
            helper.configure_profile(self.profile)
        self.assertEqual(helper.snapshot_usb(), self.baseline)
        self.profile['hid']['consumer'] = False
        helper.configure_profile(self.profile)
        self.assertTrue((self.config / 'acm.usb0').is_symlink())
        with self.assertRaisesRegex(ValueError, 'endpoint limit'):
            helper.profile_links(self.profile, True)
        helper.restore_profile()
        self.assertFalse((self.gadget / 'functions/acm.usb0').exists())
        self.assertEqual(helper.snapshot_usb(), self.baseline)

    def test_serial_disable_and_failed_apply_restore_links(self):
        self.profile['hid']['consumer'] = False
        self.profile['serial'] = True
        helper.configure_profile(self.profile)
        active = helper.snapshot_usb()
        self.profile['serial'] = False
        self.network.side_effect = [ValueError('NetworkManager unavailable'), None]
        with self.assertRaises(ValueError):
            helper.configure_profile(self.profile)
        self.assertEqual(helper.snapshot_usb(), active)
        self.assertTrue((self.gadget / 'functions/acm.usb0').exists())
        self.network.side_effect = None
        helper.configure_profile(self.profile)
        self.assertFalse((self.config / 'acm.usb0').exists())
        self.assertFalse((self.gadget / 'functions/acm.usb0').exists())

    def test_reenable_after_disabling_every_usb_function(self):
        controllers = self.root / 'udc'
        (controllers / 'controller').mkdir(parents=True)
        original = Path.write_text
        def reject_double_unbind(path, text, *args, **kwargs):
            if path == self.gadget / 'UDC' and not text.strip() and not path.read_text().strip():
                raise OSError(19, 'No such device')
            return original(path, text, *args, **kwargs)
        disabled = copy.deepcopy(self.profile)
        disabled['hid'] = dict.fromkeys(disabled['hid'], False)
        with patch.object(helper, 'UDC_ROOT', controllers), patch.object(Path, 'write_text', reject_double_unbind):
            helper.configure_profile(disabled)
            self.assertFalse((self.gadget / 'UDC').read_text().strip())
            helper.configure_profile(self.profile)
        self.assertEqual((self.gadget / 'UDC').read_text(), 'controller')

    def test_bind_failure_rolls_back_descriptors_and_links(self):
        self.profile['descriptor']['strings/0x409/product'] = 'New name'
        self.profile['serial'] = True
        self.profile['hid']['consumer'] = False
        original = Path.write_text
        failed = False
        def fail_bind(path, text, *args, **kwargs):
            nonlocal failed
            if path == self.gadget / 'UDC' and text == 'controller' and not failed:
                failed = True
                raise OSError('CONFIG_EP failed')
            return original(path, text, *args, **kwargs)
        with patch.object(Path, 'write_text', fail_bind):
            with self.assertRaises(OSError):
                helper.configure_profile(self.profile)
        self.assertEqual(helper.snapshot_usb(), self.baseline)
        self.assertEqual((self.gadget / 'UDC').read_text(), 'controller')
        self.assertIsNone(helper.saved_profile())
        self.assertFalse((self.gadget / 'functions/acm.usb0').exists())

    def test_invalid_profile_cannot_write_arbitrary_configfs_paths(self):
        for key, value in [('descriptor', {'../../etc/passwd': 'bad'}),
                           ('network', '../foo'), ('uac', 'true'), ('serial', 'true'),
                           ('hid', {'keyboard': True})]:
            profile = copy.deepcopy(self.profile)
            profile[key] = value
            with self.assertRaises(ValueError):
                helper.configure_profile(profile)
        self.assertEqual(helper.snapshot_usb(), self.baseline)

    def test_network_failure_restores_usb_before_returning_error(self):
        self.network.side_effect = [ValueError('NetworkManager unavailable'), None]
        with self.assertRaisesRegex(ValueError, 'NetworkManager'):
            helper.configure_profile(self.profile)
        self.assertEqual(helper.snapshot_usb(), self.baseline)

    def test_media_mount_retains_microphone_and_selected_hid(self):
        self.profile['hid']['consumer'] = False
        self.profile['uac'] = True
        helper.configure_profile(self.profile)
        media = self.root / 'msd'
        (media / 'images').mkdir(parents=True)
        image = media / 'images/disk.img'
        image.write_bytes(b'0' * 512)
        with patch.object(helper, 'MEDIA', media), patch.object(helper.os, 'sync'), \
                patch.object(vim4media, 'IMAGE_ROOTS', {'onekvm': media}):
            helper.configure_media(str(image), False, True)
            self.assertTrue((self.config / 'uac2.usb0').is_symlink())
            self.assertTrue((self.config / 'mass_storage.disk0').is_symlink())
            self.assertFalse((self.config / 'ncm.usb0').exists())
            helper.configure_media('', False, True, (self.lun / 'file').read_text())
            self.assertTrue((self.config / 'uac2.usb0').is_symlink())
            self.assertFalse((self.config / 'mass_storage.disk0').exists())
