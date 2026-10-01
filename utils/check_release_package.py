"""Fail if a release artifact contains a model the fork may not redistribute.

The upstream chat-bubble detector (src-python/models/ocr/onnx/chatbox_yolox_tiny.onnx)
is not under the MIT licence and must not ship in a fork's own release build
(see NOTICE.md). An entry is rejected when its file name matches, or when its
content is byte-identical to the model in this checkout, so a renamed copy is
caught too.

Usage:  python utils/check_release_package.py VRCT.zip [more.zip ...]
"""

import hashlib
import sys
import zipfile
from pathlib import PurePosixPath
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
MODEL = REPO / "src-python" / "models" / "ocr" / "onnx" / "chatbox_yolox_tiny.onnx"
FORBIDDEN_NAMES = {"chatbox_yolox_tiny.onnx"}


def _sha256(stream) -> str:
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1 << 20), b""):
        digest.update(chunk)
    return digest.hexdigest()


def find_forbidden(archive: zipfile.ZipFile, model: Path = MODEL) -> list:
    """Names of entries in `archive` that are (a copy of) the protected model."""
    model_size = model.stat().st_size if model.is_file() else None
    model_hash = _sha256(model.open("rb")) if model_size is not None else None
    found = []
    for info in archive.infolist():
        if info.is_dir():
            continue
        if PurePosixPath(info.filename).name.lower() in FORBIDDEN_NAMES:
            found.append(info.filename)
        elif model_size is not None and info.file_size == model_size:
            with archive.open(info) as stream:
                if _sha256(stream) == model_hash:
                    found.append(info.filename)
    return found


def main(paths: list) -> int:
    if not paths:
        print(__doc__)
        return 2
    failed = False
    for path in paths:
        with zipfile.ZipFile(path) as archive:
            found = find_forbidden(archive)
        if found:
            failed = True
            print(f"ERROR: {path} contains a non-redistributable model: {', '.join(found)}")
        else:
            print(f"ok: {path} contains no protected model")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
