"""WebSocket サーバーの Rust 委譲 (sink bridge) に関するテスト。

Rust ホストが VRCT_RUST_SINKS に "websocket" を載せて起動した場合、Model は
自前の WebSocketServer を作らず /internal/websocket/{start,stop,broadcast}
を stdout に出して Rust にソケットを持たせる。起動可否の判断 (ポート確認・
ワイルドカード拒否・有効/無効) は従来通り Controller/Model 側にある。
"""

import json
import os
import unittest
from threading import Lock
from unittest.mock import patch

import utils
from model import Model


class WebSocketSinkBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.model = Model.__new__(Model)
        self.lines: list = []
        patches = [
            patch.object(self.model, "_inited", True, create=True),
            patch.object(self.model, "websocket_server", None, create=True),
            patch.object(self.model, "websocket_server_alive", False, create=True),
            patch.object(self.model, "th_websocket_server", None, create=True),
            patch.object(self.model, "_websocket_lifecycle_lock", Lock(), create=True),
            patch.object(utils, "_enqueueResponseLine", self.lines.append),
            patch("model.WebSocketServer"),
            patch("model.Thread"),
            patch.dict(os.environ, {utils._SINKS_ENV: "osc,websocket"}),
        ]
        mocks = [p.start() for p in patches]
        for p in patches:
            self.addCleanup(p.stop)
        self.legacy_server, self.legacy_thread = mocks[6], mocks[7]

    def _emitted(self) -> list:
        return [(m["endpoint"], m["result"]) for m in map(json.loads, self.lines)]

    def test_start_asks_the_host_and_builds_no_python_server(self) -> None:
        self.model.startWebSocketServer("127.0.0.1", 8765)

        self.assertEqual(self._emitted(), [("/internal/websocket/start", {"host": "127.0.0.1", "port": 8765})])
        self.assertTrue(self.model.checkWebSocketServerAlive())
        self.legacy_server.assert_not_called()
        self.legacy_thread.assert_not_called()

    def test_start_while_running_does_nothing(self) -> None:
        self.model.startWebSocketServer("127.0.0.1", 8765)
        self.model.startWebSocketServer("127.0.0.1", 9999)
        self.assertEqual(len(self.lines), 1)

    def test_stop_asks_the_host_once_and_clears_the_flag(self) -> None:
        self.model.startWebSocketServer("127.0.0.1", 8765)
        self.lines.clear()

        self.model.stopWebSocketServer()
        self.model.stopWebSocketServer()

        self.assertEqual(self._emitted(), [("/internal/websocket/stop", None)])
        self.assertFalse(self.model.checkWebSocketServerAlive())

    def test_stop_without_a_running_server_emits_nothing(self) -> None:
        self.model.stopWebSocketServer()
        self.assertEqual(self.lines, [])

    def test_restart_after_stop_works(self) -> None:
        # Controller does stop() then start() when host/port change while running.
        self.model.startWebSocketServer("127.0.0.1", 8765)
        self.model.stopWebSocketServer()
        self.model.startWebSocketServer("127.0.0.1", 9000)
        self.assertEqual(
            [endpoint for endpoint, _ in self._emitted()],
            ["/internal/websocket/start", "/internal/websocket/stop", "/internal/websocket/start"],
        )
        self.assertTrue(self.model.checkWebSocketServerAlive())

    def test_broadcast_sends_the_exact_json_string_the_python_server_sent(self) -> None:
        self.model.startWebSocketServer("127.0.0.1", 8765)
        self.lines.clear()
        message = {"type": "SEND", "message": "こんにちは", "translation": ["hi"]}

        self.assertTrue(self.model.websocketSendMessage(message))

        self.assertEqual(self._emitted(), [("/internal/websocket/broadcast", {"text": json.dumps(message)})])

    def test_broadcast_while_stopped_emits_nothing(self) -> None:
        self.assertFalse(self.model.websocketSendMessage({"type": "SEND"}))
        self.assertEqual(self.lines, [])

    def test_unserialisable_broadcast_is_swallowed(self) -> None:
        self.model.startWebSocketServer("127.0.0.1", 8765)
        self.lines.clear()
        self.assertFalse(self.model.websocketSendMessage({"bad": object()}))
        self.assertEqual(self.lines, [])

    def test_without_the_host_python_keeps_its_own_server(self) -> None:
        with patch.dict(os.environ, {utils._SINKS_ENV: "osc"}):
            self.model.startWebSocketServer("127.0.0.1", 8765)
        self.legacy_thread.assert_called_once()
        self.assertEqual(self.lines, [])


if __name__ == "__main__":
    unittest.main()
