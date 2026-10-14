import importlib.machinery
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

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
        for key in ('file', 'cdrom', 'ro'):
            (lun / key).write_text('' if key == 'file' else '0')
        (gadget / 'UDC').write_text('controller')
        for name in ('hid.keyboard', 'hid.absolute', 'hid.relative', 'ncm.usb0'):
            (config / name).symlink_to(gadget / 'functions' / name)
        with patch.object(helper, 'GADGET', gadget), patch.object(helper, 'LUN', lun), \
                patch.object(helper.time, 'sleep'), patch.object(helper.os, 'sync'):
            helper.main(['media', str(self.image), 'true', 'false'])
            self.assertEqual((lun / 'ro').read_text(), '1')
            self.assertTrue((config / 'mass_storage.disk0').is_symlink())
            self.assertFalse((config / 'ncm.usb0').is_symlink())
            helper.main(['media', '', 'false', 'true'])
            self.assertTrue((config / 'ncm.usb0').is_symlink())
            self.assertFalse((config / 'mass_storage.disk0').is_symlink())
            self.assertEqual((gadget / 'UDC').read_text(), 'controller')
            for name in ('hid.keyboard', 'hid.absolute', 'hid.relative'):
                self.assertTrue((config / name).is_symlink())

    def test_rejects_unrecognized_actions_before_mutation(self):
        for args in (['reboot'], ['media', '', 'true', 'yes'], ['media']):
            with self.assertRaises(ValueError):
                helper.main(args)
