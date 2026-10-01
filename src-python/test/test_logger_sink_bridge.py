"""メッセージログの Rust 委譲 (sink bridge) に関するテスト。

Rust 側が VRCT_RUST_SINKS に "logger" を載せて起動した場合だけ、Model は
ログファイルを自分で書かず /internal/logger/* を stdout に出して Rust に書かせる。
パイプライン側の `model.logger.info(...)` はどちらの場合も同じ呼び出しのまま。
載っていない場合 (単体起動・未移植の sink) は従来通り Python が書く。
"""

import json
import os
import tempfile
import unittest
from unittest.mock import patch

import utils
from config import config
from model import Model


class LoggerSinkBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.model = Model.__new__(Model)
        self.lines: list = []
        self.logs_dir = tempfile.mkdtemp()
        self.addCleanup(self._cleanup_logs)
        stack = [
            patch.object(self.model, "_inited", True, create=True),
            patch.object(self.model, "logger", None, create=True),
            patch.object(utils, "_enqueueResponseLine", self.lines.append),
            patch.object(config, "_PATH_LOGS", self.logs_dir),
        ]
        for p in stack:
            p.start()
            self.addCleanup(p.stop)

    def _cleanup_logs(self) -> None:
        # setupLogger のハンドラはファイルを開いたままのことがあるので閉じてから消す。
        import logging
        import shutil

        logger = logging.getLogger("log")
        for handler in list(logger.handlers):
            handler.close()
            logger.removeHandler(handler)
        shutil.rmtree(self.logs_dir, ignore_errors=True)

    def _emitted(self) -> list:
        return [json.loads(line) for line in self.lines]

    def test_python_writes_the_file_itself_when_the_host_has_not_taken_it_over(self) -> None:
        for value in (None, "", "osc,websocket"):
            with self.subTest(value=value), patch.dict(os.environ, clear=False):
                os.environ.pop(utils._SINKS_ENV, None)
                if value is not None:
                    os.environ[utils._SINKS_ENV] = value
                self.model.startLogger()
                self.model.logger.info("[SENT] hello")
                self.model.stopLogger()
        self.assertEqual(self.lines, [])

    def test_host_owned_logger_forwards_instead_of_writing(self) -> None:
        with patch.dict(os.environ, {utils._SINKS_ENV: "osc,logger"}):
            self.model.startLogger()
            self.model.logger.info("[SENT] こんにちは (hello)")
            self.model.stopLogger()

        emitted = self._emitted()
        self.assertEqual(
            [(m["endpoint"], m["result"].get("text")) for m in emitted],
            [
                ("/internal/logger/start", None),
                ("/internal/logger/line", "[SENT] こんにちは (hello)"),
                ("/internal/logger/stop", None),
            ],
        )
        path = emitted[0]["result"]["path"]
        self.assertTrue(path.endswith(".log"), path)
        self.assertFalse(os.path.exists(path), "Python must not create the file the host owns")
        self.assertIsNone(self.model.logger)

    def test_sink_names_are_matched_whole(self) -> None:
        for value in ("loggerx", "xlogger", "log"):
            with self.subTest(value=value), patch.dict(os.environ, {utils._SINKS_ENV: value}):
                self.assertFalse(utils.rustSinkEnabled("logger"))


if __name__ == "__main__":
    unittest.main()
