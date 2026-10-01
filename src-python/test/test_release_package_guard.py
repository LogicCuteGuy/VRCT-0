"""リリース成果物にアップストリームの検出モデルが混入しないことを守るガードのテスト。

NOTICE.md の通り、chatbox_yolox_tiny.onnx はフォーク自身のリリースに同梱できない。
utils/check_release_package.py は、ファイル名一致と、改名されたコピー (内容一致)
の両方を拒否する。
"""

import importlib.util
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("check_release_package", ROOT / "utils" / "check_release_package.py")
guard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)


class ReleasePackageGuardTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: [p.unlink() for p in self.tmp.iterdir()] and self.tmp.rmdir())
        self.model = self.tmp / "model.onnx"
        self.model.write_bytes(b"protected-model-bytes" * 50)

    def _zip(self, entries: dict) -> zipfile.ZipFile:
        path = self.tmp / f"t{len(list(self.tmp.iterdir()))}.zip"
        with zipfile.ZipFile(path, "w") as z:
            for name, data in entries.items():
                z.writestr(name, data)
        return zipfile.ZipFile(path)

    def test_a_clean_package_passes(self) -> None:
        archive = self._zip({"VRCT.exe": b"x", "_internal/rapidocr/a.onnx": b"other model"})
        self.assertEqual(guard.find_forbidden(archive, self.model), [])

    def test_the_model_by_name_is_found_anywhere_in_the_tree(self) -> None:
        archive = self._zip({"_internal/ocr_onnx/chatbox_yolox_tiny.onnx": b"anything", "a/b/CHATBOX_YOLOX_TINY.ONNX": b"x"})
        self.assertEqual(
            sorted(guard.find_forbidden(archive, self.model)),
            ["_internal/ocr_onnx/chatbox_yolox_tiny.onnx", "a/b/CHATBOX_YOLOX_TINY.ONNX"],
        )

    def test_a_renamed_copy_is_found_by_content(self) -> None:
        archive = self._zip({"_internal/ocr_onnx/detector.bin": self.model.read_bytes()})
        self.assertEqual(guard.find_forbidden(archive, self.model), ["_internal/ocr_onnx/detector.bin"])

    def test_same_size_but_different_content_is_not_flagged(self) -> None:
        same_size = bytes(len(self.model.read_bytes()))
        archive = self._zip({"x.bin": same_size})
        self.assertEqual(guard.find_forbidden(archive, self.model), [])

    def test_the_specs_do_not_bundle_the_protected_files(self) -> None:
        # Evaluate the specs' own file selection against the real folder.
        import os
        for spec in ("backend.spec", "backend_cuda.spec"):
            text = (ROOT / "spec" / spec).read_text(encoding="utf-8")
            namespace = {"os": os, "SPECPATH": str(ROOT / "spec")}
            exec(text[text.index("_OCR_ONNX_DIR ="):text.index("a = Analysis(")], namespace)
            bundled = {Path(src).name for src, _dest in namespace["_ocr_onnx_datas"]}
            self.assertNotIn("chatbox_yolox_tiny.onnx", bundled, spec)
            self.assertFalse(bundled & {"LICENSE.txt", "LICENSE.en.txt", "NOTICE.txt"}, spec)
            self.assertNotIn("models/ocr/onnx', 'ocr_onnx/'", text, f"{spec} still bundles the whole folder")


if __name__ == "__main__":
    unittest.main()
