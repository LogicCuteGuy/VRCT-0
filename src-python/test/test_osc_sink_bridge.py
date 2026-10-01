"""OSC 送信の Rust 委譲 (sink bridge) に関するテスト。

Rust 側が VRCT_RUST_SINKS に "osc" を載せて起動した場合だけ、Model は
UDP を自分で送らず /internal/osc/* を stdout に出して Rust に送信させる。
載っていない場合 (単体起動・未移植の sink) は従来通り Python が送る。
"""

import json
import os
import unittest
from unittest.mock import MagicMock, patch

import utils
from config import config
from model import Model


class OscSinkBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        # Model はプロセス共有のシングルトン。patch.object が終了時に元へ戻す。
        self.model = Model.__new__(Model)
        self.handler = MagicMock()
        self.lines: list = []
        stack = [
            patch.object(self.model, "_inited", True, create=True),
            patch.object(self.model, "osc_handler", self.handler, create=True),
            patch.object(utils, "_enqueueResponseLine", self.lines.append),
        ]
        for p in stack:
            p.start()
            self.addCleanup(p.stop)
        self._notification = config.NOTIFICATION_VRC_SFX

    def tearDown(self) -> None:
        config.NOTIFICATION_VRC_SFX = self._notification

    def _emitted(self) -> list:
        return [json.loads(line) for line in self.lines]

    def test_python_sends_itself_when_the_host_has_not_taken_osc_over(self) -> None:
        for value in (None, "", "websocket"):
            with self.subTest(value=value), patch.dict(os.environ, clear=False):
                os.environ.pop(utils._SINKS_ENV, None)
                if value is not None:
                    os.environ[utils._SINKS_ENV] = value
                self.handler.reset_mock()
                self.model.oscSendMessage("hello")
                self.model.oscStartSendTyping()
                self.model.oscStopSendTyping()
                self.handler.sendMessage.assert_called_once()
                self.assertEqual(self.handler.sendTyping.call_count, 2)
        self.assertEqual(self.lines, [])

    def test_host_owned_osc_emits_instead_of_sending(self) -> None:
        config.NOTIFICATION_VRC_SFX = False
        with patch.dict(os.environ, {utils._SINKS_ENV: "websocket,osc"}):
            self.model.oscStartSendTyping()
            self.model.oscSendMessage("こんにちは")
            self.model.oscStopSendTyping()

        self.handler.sendMessage.assert_not_called()
        self.handler.sendTyping.assert_not_called()
        self.assertEqual(
            [(m["endpoint"], m["result"]) for m in self._emitted()],
            [
                ("/internal/osc/typing", {"flag": True}),
                ("/internal/osc/message", {"message": "こんにちは", "notification": False}),
                ("/internal/osc/typing", {"flag": False}),
            ],
        )

    def test_sink_names_are_matched_whole(self) -> None:
        # "oscx" or "xosc" must not switch the OSC sink on by substring.
        for value in ("oscx", "xosc", "osc_query"):
            with self.subTest(value=value), patch.dict(os.environ, {utils._SINKS_ENV: value}):
                self.assertFalse(utils.rustSinkEnabled("osc"))


if __name__ == "__main__":
    unittest.main()
