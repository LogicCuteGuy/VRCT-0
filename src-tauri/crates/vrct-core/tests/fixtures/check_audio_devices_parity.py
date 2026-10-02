"""Check that Rust (WASAPI through cpal) lists the same audio devices as the Python backend.

This is a property of the machine it runs on, so it is a check you run, not a golden:
it builds the lists with the repo's `DeviceManager` (PortAudio / PyAudioWPatch), runs
`cargo run -p vrct-core --example audio_devices`, and compares, for the WASAPI host:
names, channel counts, default sample rates, order, and the default devices. The default
microphone is read by Python from PortAudio's default host (MME here), which cuts names
to 31 characters, so it is compared as a prefix.

Run from anywhere:  python check_audio_devices_parity.py     (exit code 1 on a difference)
"""

import json
import subprocess
import sys
import warnings
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "src-python"))
warnings.simplefilter("ignore")

from device_manager import DeviceManager  # noqa: E402

WASAPI = "Windows WASAPI"


def python_side() -> dict:
    manager = DeviceManager()
    manager.init()
    mics = manager.getMicDevices().get(WASAPI, [])
    speakers = manager.getSpeakerDevices()
    return {
        "mics": [(d["name"], d["maxInputChannels"], int(d["defaultSampleRate"])) for d in mics if d.get("index", -1) >= 0],
        "speakers": [(d["name"], d["maxInputChannels"], int(d["defaultSampleRate"])) for d in speakers if d.get("index", -1) >= 0],
        "default_mic": manager.getDefaultMicDevice().get("device", {}).get("name"),
        "default_speaker": manager.getDefaultSpeakerDevice().get("device", {}).get("name"),
    }


def rust_side() -> dict:
    run = subprocess.run(
        ["cargo", "run", "-q", "-p", "vrct-core", "--example", "audio_devices"],
        cwd=REPO / "src-tauri", capture_output=True, text=True, encoding="utf-8", check=True,
    )
    data = json.loads(run.stdout)
    return {
        "mics": [(d["name"], d["ch"], d["rate"]) for d in data["mics"]],
        "speakers": [(d["name"], d["ch"], d["rate"]) for d in data["speakers"]],
        "default_mic": data["default_mic"],
        "default_speaker": data["default_speaker"],
    }


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    python, rust = python_side(), rust_side()
    failures = []
    for key in ("mics", "speakers"):
        if python[key] != rust[key]:
            failures.append(f"{key}: Python-only {set(python[key]) - set(rust[key])}, Rust-only {set(rust[key]) - set(python[key])}"
                            + ("" if set(python[key]) != set(rust[key]) else " (same devices, different order)"))
    if python["default_speaker"] != rust["default_speaker"]:
        failures.append(f"default speaker: Python {python['default_speaker']!r}, Rust {rust['default_speaker']!r}")
    py_mic, rs_mic = python["default_mic"] or "", rust["default_mic"] or ""
    if py_mic != rs_mic and not rs_mic.startswith(py_mic):
        failures.append(f"default mic: Python {py_mic!r}, Rust {rs_mic!r}")
    print(f"{len(rust['mics'])} microphones, {len(rust['speakers'])} speakers")
    for line in failures:
        print("DIFFERENT:", line)
    print("identical" if not failures else f"{len(failures)} difference(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
