"""Rust が代わりに返す設定ゲッターの表 (SIMPLE_GETTERS) が、Python 側の定義と
ずれていないことを固定する。

`/get/data/*` のうち「永続化される設定値をそのまま返すだけ」のものは、
src-tauri/crates/vrct-core/src/config.rs の表に載せて Rust が config の
レプリカから返す。表は controller._SIMPLE_CONFIG_GETTERS と mainloop の
エンドポイント対応、および config の永続化対象から導かれるため、どれかを
変えたのに表を更新し忘れると、Rust と Python で返す値が食い違う。
"""

import re
import unittest
from pathlib import Path

from config import json_serializable_vars
from controller import _SIMPLE_CONFIG_GETTERS

ROOT = Path(__file__).resolve().parents[2]
RUST_CONFIG = ROOT / "src-tauri" / "crates" / "vrct-core" / "src" / "config.rs"
MAINLOOP = ROOT / "src-python" / "mainloop.py"

REGEN_HINT = "Update SIMPLE_GETTERS in src-tauri/crates/vrct-core/src/config.rs"


def _rust_table() -> dict:
    source = RUST_CONFIG.read_text(encoding="utf-8")
    body = re.search(r"pub const SIMPLE_GETTERS[^=]*=\s*&\[(.*?)\n\];", source, re.S).group(1)
    return dict(re.findall(r'\("(/get/data/[a-z_0-9]+)",\s*"([A-Z_0-9]+)"\)', body))


def _expected_table() -> dict:
    mapping = re.findall(
        r'"(/get/data/[a-z_0-9]+)":\s*\{"status":\s*\w+,\s*"variable":\s*controller\.(\w+)\}',
        MAINLOOP.read_text(encoding="utf-8"),
    )
    return {
        endpoint: _SIMPLE_CONFIG_GETTERS[method]
        for endpoint, method in mapping
        if method in _SIMPLE_CONFIG_GETTERS and _SIMPLE_CONFIG_GETTERS[method] in json_serializable_vars
    }


class RustConfigGettersTests(unittest.TestCase):
    def test_table_matches_what_python_would_serve(self) -> None:
        rust, expected = _rust_table(), _expected_table()
        self.assertEqual(
            set(rust) ^ set(expected), set(),
            f"endpoints out of sync. {REGEN_HINT}",
        )
        wrong_key = {e: (rust[e], expected[e]) for e in expected if rust.get(e) != expected[e]}
        self.assertEqual(wrong_key, {}, f"endpoint reads a different config key. {REGEN_HINT}")

    def test_no_secret_is_served_by_rust(self) -> None:
        # Auth keys have their own endpoints; none may ride on the simple table.
        secrets = [key for key in _rust_table().values() if key.endswith(("AUTH_KEYS", "AUTH_TOKEN"))]
        self.assertEqual(secrets, [])

    def test_the_table_is_not_empty(self) -> None:
        # Guards the regexes above: a mapping format change must not make the
        # comparison pass vacuously.
        self.assertGreater(len(_rust_table()), 50)
        self.assertGreater(len(_expected_table()), 50)


if __name__ == "__main__":
    unittest.main()
