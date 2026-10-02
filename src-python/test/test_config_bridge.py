"""Rust ホストへの設定レプリカ通知 (config bridge) のテスト。

Config は引き続き config.json の唯一の所有者で、ホスト (src-tauri) は
`VRCT_CONFIG_BRIDGE=1` のときだけ、永続化対象の値のスナップショットと
変更通知を一方向に受け取る。ここでは「通知が出ること」「出すべきでない
ときは出ないこと」「最新値が届くこと」を固定する。
"""

import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import config as config_module
from config import Config, json_serializable_vars

SNAPSHOT = Config._BRIDGE_SNAPSHOT_ENDPOINT
CHANGED = Config._BRIDGE_CHANGED_ENDPOINT


def _isolated_config(tmpdir: str) -> Config:
    instance = object.__new__(Config)
    instance.init_config()
    instance._PATH_CONFIG = str(Path(tmpdir) / "config.json")
    instance._PATH_LOCAL = tmpdir
    return instance


class ConfigBridgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.sent: list[tuple[str, object]] = []
        patcher = mock.patch.object(
            config_module, "emitInternalMessage", lambda endpoint, result: self.sent.append((endpoint, result))
        )
        patcher.start()
        self.addCleanup(patcher.stop)

    def _enable(self) -> None:
        env = mock.patch.dict("os.environ", {Config._BRIDGE_ENV: "1"})
        env.start()
        self.addCleanup(env.stop)

    def test_silent_unless_the_host_enabled_it(self) -> None:
        cfg = _isolated_config(self._tmp.name)
        cfg.load_config()
        cfg.UI_LANGUAGE = "ja"
        self.assertEqual(self.sent, [])

    def test_load_sends_one_snapshot_of_every_persisted_value(self) -> None:
        self._enable()
        cfg = _isolated_config(self._tmp.name)
        cfg.load_config()

        snapshots = [result for endpoint, result in self.sent if endpoint == SNAPSHOT]
        self.assertEqual(len(snapshots), 1)
        self.assertEqual(set(snapshots[0]), set(json_serializable_vars))
        self.assertEqual(snapshots[0]["UI_LANGUAGE"], cfg.UI_LANGUAGE)
        # What the host mirrors must be what lands in config.json.
        on_disk = json.loads(Path(cfg.PATH_CONFIG).read_text(encoding="utf-8"))
        self.assertEqual(snapshots[0], on_disk)

    def test_snapshot_comes_after_load_time_changes(self) -> None:
        # load_config() self-heals SELECTED_RELEASE_CHANNEL; the snapshot must
        # be the last word so the host never keeps the pre-heal value.
        self._enable()
        cfg = _isolated_config(self._tmp.name)
        cfg._VERSION = "3.5.1-beta.1"
        Path(cfg.PATH_CONFIG).write_text(json.dumps({"SELECTED_RELEASE_CHANNEL": "stable"}), encoding="utf-8")
        cfg.load_config()
        self.assertEqual(self.sent[-1][0], SNAPSHOT)
        self.assertEqual(self.sent[-1][1]["SELECTED_RELEASE_CHANNEL"], "beta")

    def test_setter_sends_the_stored_value(self) -> None:
        self._enable()
        cfg = _isolated_config(self._tmp.name)
        cfg.UI_LANGUAGE = "ko"
        self.assertEqual(self.sent, [(CHANGED, {"key": "UI_LANGUAGE", "value": "ko"})])

    def test_rejected_value_sends_nothing(self) -> None:
        self._enable()
        cfg = _isolated_config(self._tmp.name)
        with self.assertRaises(config_module.ConfigValidationError):
            cfg.UI_LANGUAGE = "klingon"
        self.assertEqual(self.sent, [])

    def test_validator_normalised_value_is_what_is_sent(self) -> None:
        self._enable()
        cfg = _isolated_config(self._tmp.name)
        cfg.MIC_WORD_FILTER = ["a", "b", "a"]
        self.assertEqual(self.sent[-1], (CHANGED, {"key": "MIC_WORD_FILTER", "value": ["a", "b"]}))

    def test_non_persisted_values_are_not_mirrored(self) -> None:
        self._enable()
        cfg = _isolated_config(self._tmp.name)
        cfg.ENABLE_TRANSLATION = True
        self.assertEqual(self.sent, [])


class HostOwnedFileTests(unittest.TestCase):
    """With VRCT_CONFIG_OWNER=host the host's Settings writes config.json; Config only reports."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.sent: list[tuple[str, object]] = []
        for target, replacement in (
            ("emitInternalMessage", lambda endpoint, result: self.sent.append((endpoint, result))),
        ):
            patcher = mock.patch.object(config_module, target, replacement)
            patcher.start()
            self.addCleanup(patcher.stop)
        env = mock.patch.dict("os.environ", {Config._BRIDGE_ENV: "1", Config._OWNER_ENV: "host"})
        env.start()
        self.addCleanup(env.stop)

    def test_load_and_saves_leave_the_file_alone(self) -> None:
        cfg = _isolated_config(self._tmp.name)
        path = Path(cfg.PATH_CONFIG)
        path.write_text(json.dumps({"UI_LANGUAGE": "ja"}), encoding="utf-8")
        before = path.read_bytes()

        cfg.load_config()
        cfg.UI_LANGUAGE = "ko"
        cfg.saveConfigToFile()

        self.assertEqual(path.read_bytes(), before)
        self.assertEqual(cfg.UI_LANGUAGE, "ko")

    def test_no_file_is_created(self) -> None:
        cfg = _isolated_config(self._tmp.name)
        cfg.load_config()
        cfg.saveConfigToFile()
        self.assertFalse(Path(cfg.PATH_CONFIG).exists())

    def test_changes_are_still_reported(self) -> None:
        cfg = _isolated_config(self._tmp.name)
        cfg.load_config()
        self.sent.clear()
        cfg.UI_LANGUAGE = "ko"
        self.assertEqual(self.sent, [(CHANGED, {"key": "UI_LANGUAGE", "value": "ko"})])

    def test_any_other_owner_value_keeps_python_writing(self) -> None:
        with mock.patch.dict("os.environ", {Config._OWNER_ENV: "python"}):
            cfg = _isolated_config(self._tmp.name)
            cfg.load_config()
        self.assertTrue(Path(cfg.PATH_CONFIG).exists())


if __name__ == "__main__":
    unittest.main()
