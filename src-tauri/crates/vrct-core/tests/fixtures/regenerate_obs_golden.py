"""Regenerate the OBS browser source page template and golden cases from the
real Python `models/obs/obs_browser_source_server.py`.

Writes two files:

* `src/sinks/obs/page.html` -- the page with `@@NAME@@` placeholders. Python's
  own f-string is rendered with placeholder values, so the template is the
  Python output, not a re-typed copy. (Once the Python server is deleted this
  file is the source of truth and this script goes away.)
* `tests/fixtures/obs_golden.json` -- the page Python renders for a range of
  config values (defaults, typical, clamped, bad colours, strings, floats,
  nulls), plus the status and headers of a real running Python server.

Run from anywhere:  python regenerate_obs_golden.py
"""

import json
import sys
import threading
import urllib.error
import urllib.request
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / "src-python"))

import models.obs.obs_browser_source_server as server_module  # noqa: E402

TEMPLATE = REPO / "src-tauri" / "crates" / "vrct-core" / "src" / "sinks" / "obs" / "page.html"


class Placeholder(str):
    """Formats as itself, so the f-string output carries `@@NAME@@`."""


def render_template() -> str:
    by_range = {
        (1, 65535): "WS_PORT",
        (1, 50): "MAX_MESSAGES",
        (1, 120): "DISPLAY_DURATION",
        (0, 120): "FADEOUT_DURATION",
        (10, 200): "FONT_SIZE",
        (0, 20): "OUTLINE_THICKNESS",
    }

    def clamp(_value, low, high):
        return Placeholder(f"@@{by_range[(low, high)]}@@")

    def color(_value, fallback="#FFFFFF"):
        return Placeholder("@@OUTLINE_COLOR@@" if fallback == "#000000" else "@@FONT_COLOR@@")

    with mock.patch.object(server_module, "_clamp_int", clamp), mock.patch.object(
        server_module, "_normalize_hex_color", color
    ):
        return server_module._build_overlay_html("@@WS_TOKEN@@")


def render(config: dict, token: str) -> str:
    with mock.patch.object(server_module, "config", SimpleNamespace(**config)):
        return server_module._build_overlay_html(token)


CASES = [
    ("defaults", {}, "tok"),
    (
        "typical",
        dict(
            WEBSOCKET_PORT=2299,
            OBS_BROWSER_SOURCE_MAX_MESSAGES=20,
            OBS_BROWSER_SOURCE_DISPLAY_DURATION=30,
            OBS_BROWSER_SOURCE_FADEOUT_DURATION=5,
            OBS_BROWSER_SOURCE_FONT_SIZE=64,
            OBS_BROWSER_SOURCE_FONT_COLOR="#00ff88",
            OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS=5,
            OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR="#112233",
        ),
        "abc_DEF-123",
    ),
    (
        "clamped_low",
        dict(
            WEBSOCKET_PORT=0,
            OBS_BROWSER_SOURCE_MAX_MESSAGES=0,
            OBS_BROWSER_SOURCE_DISPLAY_DURATION=0,
            OBS_BROWSER_SOURCE_FADEOUT_DURATION=-5,
            OBS_BROWSER_SOURCE_FONT_SIZE=1,
            OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS=-1,
        ),
        "",
    ),
    (
        "clamped_high",
        dict(
            WEBSOCKET_PORT=99999,
            OBS_BROWSER_SOURCE_MAX_MESSAGES=999,
            OBS_BROWSER_SOURCE_DISPLAY_DURATION=9999,
            OBS_BROWSER_SOURCE_FADEOUT_DURATION=9999,
            OBS_BROWSER_SOURCE_FONT_SIZE=9999,
            OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS=999,
        ),
        "tok",
    ),
    (
        "bad_colors",
        dict(OBS_BROWSER_SOURCE_FONT_COLOR="red", OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR="#12345"),
        "tok",
    ),
    (
        "bad_colors_2",
        dict(OBS_BROWSER_SOURCE_FONT_COLOR="#GGGGGG", OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR=123),
        "tok",
    ),
    (
        "null_colors",
        dict(OBS_BROWSER_SOURCE_FONT_COLOR=None, OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR=None),
        "tok",
    ),
    (
        "padded_color",
        dict(OBS_BROWSER_SOURCE_FONT_COLOR="  #AbCdEf ", OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR="#abcdef\n"),
        "tok",
    ),
    (
        "strings_and_floats",
        dict(
            WEBSOCKET_PORT="2231",
            OBS_BROWSER_SOURCE_MAX_MESSAGES="7",
            OBS_BROWSER_SOURCE_DISPLAY_DURATION=2.9,
            OBS_BROWSER_SOURCE_FADEOUT_DURATION="12.5",
            OBS_BROWSER_SOURCE_FONT_SIZE=True,
            OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS="3 ",
        ),
        "tok",
    ),
    (
        "nulls",
        dict(
            WEBSOCKET_PORT=None,
            OBS_BROWSER_SOURCE_MAX_MESSAGES=None,
            OBS_BROWSER_SOURCE_DISPLAY_DURATION=None,
            OBS_BROWSER_SOURCE_FADEOUT_DURATION=None,
            OBS_BROWSER_SOURCE_FONT_SIZE=None,
            OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS=None,
        ),
        "tok",
    ),
]


def capture_http() -> list:
    """Status and headers a real running Python server answers with."""
    with mock.patch.object(server_module, "config", SimpleNamespace()):
        server = server_module.ObsBrowserSourceServer("127.0.0.1", 0, "tok")
        # Port 0 asks the OS for a free port; read it back after binding.
        server.port = 0
        server.start()
        port = server._server.server_address[1]
        out = []
        try:
            for path in ["/", "/obs", "/obs?x=1", "/health", "/nope", "/health/", "/OBS"]:
                try:
                    response = urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=5)
                except urllib.error.HTTPError as error:
                    response = error
                body = response.read()
                out.append(
                    {
                        "path": path,
                        "status": response.status if hasattr(response, "status") else response.code,
                        "content_type": response.headers.get("Content-Type"),
                        "cache_control": response.headers.get("Cache-Control"),
                        "body_is_page": body.decode("utf-8", "replace").startswith("<!doctype html>"),
                        "body": None if body.startswith(b"<!doctype") else body.decode("utf-8"),
                    }
                )
        finally:
            server.stop()
        return out


template = render_template()
placeholders = {part.split("@@")[0] for part in template.split("@@")[1::2]}
assert placeholders == {
    "WS_PORT", "MAX_MESSAGES", "DISPLAY_DURATION", "FADEOUT_DURATION",
    "FONT_SIZE", "FONT_COLOR", "OUTLINE_THICKNESS", "OUTLINE_COLOR", "WS_TOKEN",
}, placeholders
assert template.count("@@") % 2 == 0

TEMPLATE.parent.mkdir(parents=True, exist_ok=True)
TEMPLATE.write_text(template, encoding="utf-8", newline="\n")

cases = [
    {"name": name, "config": config, "token": token, "html": render(config, token)}
    for name, config, token in CASES
]
golden = {"cases": cases, "http": capture_http()}
out = HERE / "obs_golden.json"
out.write_text(json.dumps(golden, ensure_ascii=False, indent=1) + "\n", encoding="utf-8", newline="\n")
print(f"wrote {TEMPLATE.name} ({len(template)} bytes) and {len(cases)} cases + {len(golden['http'])} http probes to {out.name}")
