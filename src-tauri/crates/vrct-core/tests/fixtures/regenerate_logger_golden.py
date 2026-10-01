"""Regenerate logger_golden.json from the real Python message logger.

The Rust logger sink must write the lines `setupLogger` (a
`TruncatingFileHandler` + `'%(asctime)s - %(name)s - %(levelname)s - %(message)s'`)
wrote, so the expected text is captured by logging through the real thing.
The timestamp is replaced by `<TS>`, and line ends are stored as `\\n` (the
Rust test converts them to the platform's, as Python's text mode did).

Run from anywhere:  python regenerate_logger_golden.py
"""

import json
import os
import re
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "src-python"))

from utils import setupLogger  # noqa: E402

MESSAGES = [
    "[SENT] hello",
    "[RECEIVED] こんにちは (hello/你好)",
    "[CHAT] line one\nline two",
    "",
    "[SENT] 🙂 emoji and 100% done {braces} %s",
]

STAMP = re.compile(rb"^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d,\d{3}", re.MULTILINE)

with tempfile.TemporaryDirectory() as directory:
    path = os.path.join(directory, "golden.log")
    logger = setupLogger("log", path)
    for message in MESSAGES:
        logger.info(message)
    for handler in logger.handlers:
        handler.close()
    raw = Path(path).read_bytes()

assert raw.count(b"\r\n") == raw.count(b"\n") or os.linesep == "\n", "mixed line ends"
normalized = STAMP.sub(b"<TS>", raw.replace(b"\r\n", b"\n")).decode("utf-8")

out = Path(__file__).with_name("logger_golden.json")
out.write_text(
    json.dumps({"messages": MESSAGES, "file": normalized}, ensure_ascii=False, indent=1) + "\n",
    encoding="utf-8",
)
print(f"wrote {len(MESSAGES)} records to {out}")
