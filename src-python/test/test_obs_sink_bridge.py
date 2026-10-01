"""OBS ブラウザソースの HTTP サーバーの Rust 委譲 (sink bridge) に関するテスト。

Rust 側が VRCT_RUST_SINKS に "obs" を載せて起動した場合だけ、Model は自分で
HTTP サーバーを立てず /internal/obs/{start,stop} を stdout に出す。ページは
Rust が config レプリカから生成する。ポート確認などの判断は従来通り呼び出し側。
載っていない場合 (単体起動・未移植の sink) は従来通り Python が提供する。
"""

import json
import os
import unittest
from unittest.mock import PropertyMock, patch

import utils
from model import Model, _HostObsServer
from models.obs.obs_browser_source_server import ObsBrowserSourceServer


class ObsSinkBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.model = Model.__new__(Model)
        self.lines: list = []
        stack = [
            patch.object(self.model, "_inited", True, create=True),
            patch.object(self.model, "obs_browser_source_server", None, create=True),
            patch.object(utils, "_enqueueResponseLine", self.lines.append),
            # No test here may open a real socket, even if the gate is broken.
            patch.object(ObsBrowserSourceServer, "start"),
            patch.object(ObsBrowserSourceServer, "stop"),
        ]
        for p in stack:
            p.start()
            self.addCleanup(p.stop)

    def _emitted(self) -> list:
        return [(m["endpoint"], m["result"]) for m in map(json.loads, self.lines)]

    def test_python_serves_itself_when_the_host_has_not_taken_it_over(self) -> None:
        for value in (None, "", "osc,websocket,logger,clipboard"):
            with self.subTest(value=value), patch.dict(os.environ, clear=False):
                os.environ.pop(utils._SINKS_ENV, None)
                if value is not None:
                    os.environ[utils._SINKS_ENV] = value
                # The real class with its socket work stubbed out.
                with patch.object(ObsBrowserSourceServer, "start") as start, patch.object(
                    ObsBrowserSourceServer, "stop"
                ) as stop, patch.object(
                    ObsBrowserSourceServer, "is_running", new_callable=PropertyMock, return_value=True
                ):
                    self.model.startObsBrowserSourceServer("127.0.0.1", 2232)
                    start.assert_called_once()
                    self.assertTrue(self.model.checkObsBrowserSourceServerAlive())
                    self.model.stopObsBrowserSourceServer()
                    stop.assert_called_once()
        self.assertEqual(self.lines, [])

    def test_host_owned_obs_emits_start_and_stop_and_tracks_liveness(self) -> None:
        with patch.dict(os.environ, {utils._SINKS_ENV: "logger,obs"}):
            self.assertFalse(self.model.checkObsBrowserSourceServerAlive())
            self.model.startObsBrowserSourceServer("127.0.0.1", 2232)
            self.assertTrue(self.model.checkObsBrowserSourceServerAlive())
            self.model.stopObsBrowserSourceServer()
            self.assertFalse(self.model.checkObsBrowserSourceServerAlive())

        self.assertEqual(
            self._emitted(),
            [
                ("/internal/obs/start", {"host": "127.0.0.1", "port": 2232}),
                ("/internal/obs/stop", None),
            ],
        )
        self.assertIsNone(self.model.obs_browser_source_server)

    def test_starting_the_same_address_again_does_not_restart_the_host_server(self) -> None:
        with patch.dict(os.environ, {utils._SINKS_ENV: "obs"}):
            self.model.startObsBrowserSourceServer("127.0.0.1", 2232)
            self.model.startObsBrowserSourceServer("127.0.0.1", 2232)
        self.assertEqual([endpoint for endpoint, _ in self._emitted()], ["/internal/obs/start"])

    def test_a_new_port_stops_then_starts(self) -> None:
        with patch.dict(os.environ, {utils._SINKS_ENV: "obs"}):
            self.model.startObsBrowserSourceServer("127.0.0.1", 2232)
            self.model.startObsBrowserSourceServer("127.0.0.1", 2240)
        self.assertEqual(
            self._emitted(),
            [
                ("/internal/obs/start", {"host": "127.0.0.1", "port": 2232}),
                ("/internal/obs/stop", None),
                ("/internal/obs/start", {"host": "127.0.0.1", "port": 2240}),
            ],
        )
        self.assertIsInstance(self.model.obs_browser_source_server, _HostObsServer)
        self.assertEqual(self.model.obs_browser_source_server.port, 2240)

    def test_sink_names_are_matched_whole(self) -> None:
        for value in ("obsx", "xobs", "obs_browser_source"):
            with self.subTest(value=value), patch.dict(os.environ, {utils._SINKS_ENV: value}):
                self.assertFalse(utils.rustSinkEnabled("obs"))


if __name__ == "__main__":
    unittest.main()
