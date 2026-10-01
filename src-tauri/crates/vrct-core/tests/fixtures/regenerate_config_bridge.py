"""Regenerate config_bridge.jsonl from the real Python `Config`.

The Rust side must understand exactly what the sidecar writes, so the fixture is
captured through the real `emitInternalMessage` path, not hand-written. Machine
specific values (audio devices, the random WebSocket token) are replaced so the
committed file is stable and leaks nothing.

Run from anywhere:  python regenerate_config_bridge.py
"""

import json
import os
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "src-python"))
os.environ["VRCT_CONFIG_BRIDGE"] = "1"

import utils  # noqa: E402

captured: list[str] = []
# emitInternalMessage still builds the line itself; only the stdout queue is swapped.
utils._enqueueResponseLine = captured.append

from config import Config  # noqa: E402

REDACTED = {
    "SELECTED_MIC_HOST": "NoHost",
    "SELECTED_MIC_DEVICE": "NoDevice",
    "SELECTED_SPEAKER_DEVICE": "NoDevice",
    "WEBSOCKET_AUTH_TOKEN": "redacted-token",
    "SELECTED_TRANSLATION_COMPUTE_DEVICE": {
        "device": "cpu", "device_index": 0, "device_name": "cpu",
        "compute_types": ["auto", "float32", "int16", "int8", "int8_float32"],
    },
}
REDACTED["SELECTED_TRANSCRIPTION_COMPUTE_DEVICE"] = REDACTED["SELECTED_TRANSLATION_COMPUTE_DEVICE"]

# Importing `config` built the process-wide singleton from the developer's own
# config.json; only what the isolated instance below sends is kept.
captured.clear()
tmp = tempfile.mkdtemp()
cfg = object.__new__(Config)
cfg.init_config()
cfg._PATH_CONFIG = os.path.join(tmp, "config.json")
cfg._PATH_LOCAL = tmp
cfg.load_config()
cfg.UI_LANGUAGE = "ja"
cfg.MIC_WORD_FILTER = ["a", "a", "b"]

messages = [json.loads(line) for line in captured]
last_snapshot = max(i for i, m in enumerate(messages) if m["endpoint"] == Config._BRIDGE_SNAPSHOT_ENDPOINT)
messages = messages[last_snapshot:]
messages[0]["result"].update(REDACTED)

out = Path(__file__).with_name("config_bridge.jsonl")
out.write_text("".join(json.dumps(m, ensure_ascii=False) + "\n" for m in messages), encoding="utf-8")
print(f"wrote {len(messages)} lines to {out}")
