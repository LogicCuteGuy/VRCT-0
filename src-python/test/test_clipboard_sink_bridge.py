"""クリップボード貼り付けの Rust 委譲 (sink bridge) に関するテスト。

Rust 側が VRCT_RUST_SINKS に "clipboard" を載せて起動した場合だけ、Model は
自分でコピー/フォーカス/Ctrl+V をせず /internal/clipboard/copy_paste を出す。
SteamVR のアプリ名は OpenVR 由来なので Python が付けて渡す。
載っていない場合 (単体起動・Windows 以外) は従来通り Python が貼り付ける。
"""

import json
import os
import unittest
from unittest.mock import MagicMock, patch

import utils
from model import Model
from models.clipboard.clipboard import Clipboard


class ClipboardSinkBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.model = Model.__new__(Model)
        self.clipboard = MagicMock(spec=Clipboard)
        self.clipboard.app_name = "VRChat"
        self.lines: list = []
        stack = [
            patch.object(self.model, "_inited", True, create=True),
            patch.object(self.model, "clipboard", self.clipboard, create=True),
            patch.object(utils, "_enqueueResponseLine", self.lines.append),
        ]
        for p in stack:
            p.start()
            self.addCleanup(p.stop)

    def _emitted(self) -> list:
        return [json.loads(line) for line in self.lines]

    def test_python_pastes_itself_when_the_host_has_not_taken_it_over(self) -> None:
        for value in (None, "", "osc,websocket,logger"):
            with self.subTest(value=value), patch.dict(os.environ, clear=False):
                os.environ.pop(utils._SINKS_ENV, None)
                if value is not None:
                    os.environ[utils._SINKS_ENV] = value
                self.clipboard.reset_mock()
                self.assertTrue(self.model.setCopyToClipboardAndPasteFromClipboard("hello"))
                self.clipboard.copy_and_paste.assert_called_once_with("hello")
        self.assertEqual(self.lines, [])

    def test_host_owned_clipboard_emits_the_text_and_the_game_name(self) -> None:
        with patch.dict(os.environ, {utils._SINKS_ENV: "logger,clipboard"}):
            self.assertTrue(self.model.setCopyToClipboardAndPasteFromClipboard("こんにちは"))

        self.clipboard.copy_and_paste.assert_not_called()
        self.assertEqual(
            [(m["endpoint"], m["result"]) for m in self._emitted()],
            [("/internal/clipboard/copy_paste", {"text": "こんにちは", "window": "VRChat"})],
        )

    def test_no_game_running_sends_no_window(self) -> None:
        self.clipboard.app_name = None
        with patch.dict(os.environ, {utils._SINKS_ENV: "clipboard"}):
            self.model.setCopyToClipboardAndPasteFromClipboard("hi")
        self.assertEqual(self._emitted()[0]["result"], {"text": "hi", "window": None})

    def test_sink_names_are_matched_whole(self) -> None:
        for value in ("clipboardx", "xclipboard", "clip"):
            with self.subTest(value=value), patch.dict(os.environ, {utils._SINKS_ENV: value}):
                self.assertFalse(utils.rustSinkEnabled("clipboard"))


if __name__ == "__main__":
    unittest.main()
