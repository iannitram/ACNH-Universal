import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
import zipfile
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("setup_acnh", Path(__file__).resolve().parents[1] / "setup_acnh.py")
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class FanMods(unittest.TestCase):
    def test_incomplete_archive_preserves_the_prepared_translation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            installed = root / "mods/ptbr/romfs/previous.txt"
            installed.parent.mkdir(parents=True)
            installed.write_text("previous translation")
            archive = root / "ptbr.zip"
            with zipfile.ZipFile(archive, "w") as z:
                z.writestr("romfs/", "")
            with self.assertRaises(SystemExit):
                setup.install_mod_archive(str(root / "mods"), "ptbr", str(archive), str(root))
            self.assertEqual(installed.read_text(), "previous translation")
            self.assertTrue(archive.is_file())

    def test_translation_opt_in_then_off_preserves_inputs_and_other_mods(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            game, out = root / "game", root / "out"
            game.mkdir()
            out.mkdir()
            archive = game / "ptbr.zip"
            with zipfile.ZipFile(archive, "w") as z:
                z.writestr("romfs/synthetic.txt", "translation")
            other = out / "mods/other/romfs/file"
            other.parent.mkdir(parents=True)
            other.write_text("other")
            with patch.dict(os.environ, {"ACNH_RUST_MSG_REFLOW": "1"}):
                setup.install_mods(str(out), str(root), str(game), False, [])
                self.assertFalse((out / "mods/ptbr").exists())
                setup.install_mods(str(out), str(root), str(game), True, [])
                self.assertEqual((out / "mods/ptbr/romfs/synthetic.txt").read_text(), "translation")
                setup.install_mods(str(out), str(root), str(game), False, [])
            self.assertFalse((out / "mods/ptbr").exists())
            self.assertTrue(archive.is_file())
            self.assertEqual(other.read_text(), "other")

    def test_patch_selection_preserves_sources_and_rejects_old_output_after_game_change(self):
        with tempfile.TemporaryDirectory() as tmp:
            game, out = Path(tmp) / "game", Path(tmp) / "out"
            game.mkdir()
            out.mkdir()
            old = out / "speedfix_main.pchtxt"
            old.write_text("synthetic old patch")
            self.assertEqual(setup.speedfix_patch(str(game), str(out), "3.0.3", False), str(old))
            self.assertIsNone(setup.speedfix_patch(str(game), str(out), "3.0.3", True))
            patch302 = game / "speedfix_main_302.pchtxt"
            patch302.write_text("synthetic patch")
            self.assertEqual(setup.speedfix_patch(str(game), str(out), "3.0.2", True), str(patch302))
            self.assertTrue(old.is_file() and patch302.is_file())
            self.assertEqual(setup.speedfix_args(False, "main", "3.0.2", str(game), str(out), True), [])
            self.assertEqual(setup.speedfix_args(True, "main", "3.0.2", str(game), str(out), True),
                             ["--variant", str(patch302)])
            for version in ("1.0.0", "2.0.7"):
                self.assertEqual(setup.speedfix_args(True, "main", version, str(game), str(out), True), [])
            self.assertEqual(setup.speedfix_args(True, "sdk", "3.0.2", str(game), str(out), True), [])


if __name__ == "__main__":
    unittest.main()
