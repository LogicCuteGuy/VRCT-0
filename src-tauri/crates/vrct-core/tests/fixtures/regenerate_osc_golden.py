"""Regenerate osc_golden.json from the real Python `OSCHandler`.

The Rust OSC sink must put the same bytes on the wire as python-osc did, so
the expected packets are captured by calling the real handler with a socket
that records instead of sending.

Run from anywhere:  python regenerate_osc_golden.py
"""

import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "src-python"))

from models.osc.osc import OSCHandler  # noqa: E402


class RecordingSocket:
    def __init__(self) -> None:
        self.packets: list[bytes] = []

    def sendto(self, data: bytes, address) -> int:
        self.packets.append(bytes(data))
        return len(data)


handler = OSCHandler("127.0.0.1", 9000)
recorder = RecordingSocket()
handler.udp_client._sock = recorder

# Lengths 0-5 cross every 4-byte padding case; the rest cover non-ASCII text.
MESSAGES = ["", "a", "ab", "abc", "abcd", "abcde", "Hello World", "こんにちは", "hello\nこんにちは 🙂"]

cases = []
for flag in (True, False):
    recorder.packets.clear()
    handler.sendTyping(flag=flag)
    cases.append({"kind": "typing", "flag": flag, "packet": recorder.packets[0].hex()})

for message in MESSAGES:
    for notification in (True, False):
        recorder.packets.clear()
        handler.sendMessage(message=message, notification=notification)
        cases.append({
            "kind": "message",
            "message": message,
            "notification": notification,
            # python-osc's wrapper sends nothing for an empty message.
            "packet": recorder.packets[0].hex() if recorder.packets else None,
        })

out = Path(__file__).with_name("osc_golden.json")
out.write_text(json.dumps(cases, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
print(f"wrote {len(cases)} cases to {out}")
