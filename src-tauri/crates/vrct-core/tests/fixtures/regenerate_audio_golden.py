"""Regenerate the audio goldens from the real Python implementation.

`audio_normalizer_golden.json` -- `Pcm16MonoNormalizer.process` (numpy channel
mean + `audioop.lin2lin` + `audioop.ratecv`) over chunk sequences, so the state
`ratecv` carries between chunks is covered too.

`audio_vad_golden.json` -- `VadSegmenter` driven with a scripted probability
engine. The PCM is a formula (`frame_bytes`), so the file stores only a start
frame, a frame count and a number of trailing bytes per step. What is recorded:
the segments (length, SHA-256, id relative to the first one, reason), the
`speaking` flag after each step, a fingerprint of every frame handed to the
engine, the number of engine resets and every diagnostic line.

Python 3.12's `audioop` is the same C code as the 3.11 the app ships with.

Run from anywhere:  python regenerate_audio_golden.py
"""

import hashlib
import json
import random
import re
import struct
import sys
import warnings
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "src-python"))

import numpy as np  # noqa: E402

warnings.simplefilter("ignore", DeprecationWarning)

from models.transcription.audio_vad import FRAME_SAMPLES, Pcm16MonoNormalizer, VadSegmenter  # noqa: E402

HERE = Path(__file__).resolve().parent


# ---- normalizer ------------------------------------------------------------

def random_bytes(rng: random.Random, count: int) -> bytes:
    return bytes(rng.randrange(256) for _ in range(count))


def tone_bytes(width: int, channels: int, frames: int, phase: int) -> bytes:
    """A slow ramp with a few extremes; closer to audio than noise, so interpolation is exercised."""
    top = (1 << (8 * width - 1)) - 1
    out = bytearray()
    for n in range(frames):
        for c in range(channels):
            value = int(top * 0.8 * ((((n + phase) * (c + 3)) % 97) / 48.0 - 1.0))
            if n % 41 == 0:
                value = top if c % 2 == 0 else -top - 1
            out += value.to_bytes(width, "little", signed=True)
    return bytes(out)


def normalizer_cases() -> list:
    rng = random.Random(20260416)
    specs = [
        # (rate, width, channels, chunk sizes in whole frames, extra trailing bytes per chunk)
        (16000, 2, 1, [160, 3, 0, 640], [0, 0, 0, 0]),
        (44100, 2, 1, [441, 441, 100, 1, 2000], [0, 0, 0, 0, 0]),
        (48000, 2, 1, [480, 480, 7, 960], [0, 0, 0, 0]),
        (48000, 2, 2, [480, 480, 1, 2], [0, 0, 0, 0]),
        (44100, 2, 2, [441, 5, 441], [0, 0, 0]),
        (22050, 2, 1, [220, 221, 1000], [0, 0, 0]),
        (8000, 2, 1, [80, 80, 3, 400], [0, 0, 0, 0]),
        (11025, 2, 1, [331, 11], [0, 0]),
        (96000, 2, 2, [960, 960, 31], [0, 0, 0]),
        (32000, 2, 1, [320, 320], [0, 0]),
        (12345, 2, 1, [500, 77, 500], [0, 0, 0]),
        (16000, 2, 3, [100, 100], [0, 0]),
        (48000, 2, 4, [480, 17], [0, 0]),
        (16000, 2, 0, [50], [0]),
        (16000, 1, 1, [160, 160], [0, 0]),
        (44100, 1, 2, [441, 100], [0, 0]),
        (16000, 3, 1, [160, 160], [0, 0]),
        (48000, 3, 2, [480, 121], [0, 0]),
        (16000, 4, 1, [160, 160], [0, 0]),
        (44100, 4, 2, [441, 3, 441], [0, 0, 0]),
        # Trailing bytes: whole samples of a partial stereo frame are dropped, odd bytes are errors.
        (48000, 2, 2, [100, 100, 100], [2, 0, 6]),
        (16000, 2, 1, [100, 100], [1, 0]),
        (44100, 2, 1, [441, 441], [1, 0]),
        (16000, 3, 1, [100, 100], [1, 0]),
        (16000, 4, 1, [100, 100], [2, 0]),
        (16000, 5, 1, [100], [0]),
    ]
    cases = []
    for index, (rate, width, channels, sizes, extras) in enumerate(specs):
        normalizer = Pcm16MonoNormalizer(rate, width, channels)
        real_channels = max(1, channels)
        chunks = []
        for number, (frames, extra) in enumerate(zip(sizes, extras)):
            if index % 2 == 0:
                data = tone_bytes(width, real_channels, frames, number * 13)
            else:
                data = random_bytes(rng, frames * width * real_channels)
            data += random_bytes(rng, extra)
            try:
                out = normalizer.process(data).hex()
                error = False
            except Exception:
                out, error = "", True
            chunks.append({"input": data.hex(), "output": out, "error": error})
        cases.append({"sample_rate": rate, "sample_width": width, "channels": channels, "chunks": chunks})

    # Empty input, and a reset between chunks (state is dropped).
    normalizer = Pcm16MonoNormalizer(44100, 2, 1)
    steps = []
    for data in (tone_bytes(2, 1, 441, 0), b"", tone_bytes(2, 1, 441, 5), "reset", tone_bytes(2, 1, 441, 5)):
        if data == "reset":
            normalizer.reset()
            steps.append({"reset": True})
            continue
        steps.append({"input": data.hex(), "output": normalizer.process(data).hex(), "error": False})
    return {"cases": cases, "reset_case": {"sample_rate": 44100, "sample_width": 2, "channels": 1, "steps": steps}}


# ---- segmenter -------------------------------------------------------------

def frame_bytes(k: int) -> bytes:
    return b"".join(struct.pack("<h", ((k * 977 + i * 31 + 12345) % 65536) - 32768) for i in range(FRAME_SAMPLES))


def pcm(start: int, frames: int, extra: int) -> bytes:
    data = b"".join(frame_bytes(start + n) for n in range(frames))
    return data + frame_bytes(start + frames)[:extra]


def runs(*spec) -> list:
    out = []
    for value, count in spec:
        out += [float(np.float32(value))] * count
    return out


class Engine:
    def __init__(self, probs):
        self.probs = list(probs)
        self.used = []
        self.calls = []
        self.resets = 0

    def __call__(self, frame):
        index = len(self.used)
        value = self.probs[index] if index < len(self.probs) else 0.0
        self.used.append(value)
        self.calls.append([float(frame[0]), float(frame[255]), float(frame[FRAME_SAMPLES - 1])])
        return value

    def reset(self):
        self.resets += 1


DEFAULTS = {
    "speech_threshold": 0.25,
    "negative_threshold": None,
    "hangover_frames": 24,
    "max_speech_frames": 250,
    "min_speech_frames": 2,
    "pre_speech_pad_frames": 5,
}


def describe(segment, base: int) -> dict:
    return {
        "len": len(segment.audio), "sha256": hashlib.sha256(segment.audio).hexdigest(),
        "id": segment.segment_id - base, "reason": segment.reason,
    }


def run_scenario(name: str, probs: list, steps: list, **overrides) -> dict:
    params = {**DEFAULTS, **overrides}
    engine = Engine(probs)
    logs = []
    segmenter = VadSegmenter(engine, diagnostic_callback=logs.append, diagnostic_label="mic", **params)
    base = segmenter._segment_id
    recorded = []
    for step in steps:
        op = step["op"]
        result = {"op": op}
        if op == "process":
            segments = segmenter.process(pcm(step["start"], step["frames"], step.get("extra", 0)))
            result["segments"] = [describe(s, base) for s in segments]
        elif op == "flush":
            segment = segmenter.flush()
            result["segment"] = None if segment is None else describe(segment, base)
        elif op == "reset":
            segmenter.reset()
        result["speaking"] = segmenter.speaking
        result["engine_calls"] = len(engine.used)
        result["engine_resets"] = engine.resets
        recorded.append({**step, "result": result})
    # Python's id counter is process wide; the log lines carry it, so make them relative like the segments.
    logs = [re.sub(r"segment_id=(\d+)", lambda m: f"segment_id={int(m.group(1)) - base}", line) for line in logs]
    return {
        "name": name, "params": params, "probs": engine.used, "steps": recorded, "calls": engine.calls, "logs": logs,
    }


def process(start: int, frames: int, extra: int = 0) -> dict:
    return {"op": "process", "start": start, "frames": frames, "extra": extra}


def scenarios() -> list:
    out = []

    def add(name, probs, steps, **overrides):
        out.append(run_scenario(name, probs, steps, **overrides))

    add("basic_silence_end", runs((0.02, 3), (0.9, 10), (0.02, 30)), [process(0, 43)])
    add("single_blip_no_start", runs((0.9, 1), (0.02, 5), (0.9, 1), (0.02, 5)), [process(0, 12)])
    add("exact_threshold_starts", runs((0.25, 2), (0.0999, 40)), [process(0, 42)])
    add("mid_band_never_ends", runs((0.9, 3), (0.1, 60), (0.0999, 30)), [process(0, 93)])
    add("mid_band_does_not_move_anchor", runs((0.9, 4), (0.15, 5), (0.05, 10), (0.15, 5), (0.05, 30)), [process(0, 54)])
    add("recover_from_silence", runs((0.9, 4), (0.02, 10), (0.9, 3), (0.02, 30)), [process(0, 47)])
    add("max_split_continuous", runs((0.9, 70), (0.02, 30)), [process(0, 100)], max_speech_frames=20)
    add("max_reached_at_start", runs((0.9, 6), (0.02, 3)), [process(0, 9)], max_speech_frames=2)
    add("max_none_never_splits", runs((0.9, 300), (0.02, 30)), [process(0, 330)], max_speech_frames=None)
    add("two_utterances", runs((0.9, 5), (0.02, 30), (0.9, 5), (0.02, 30)), [process(0, 35), process(35, 35)])
    add("flush_mid_speech", runs((0.9, 10)), [process(0, 10), {"op": "flush"}, {"op": "flush"}])
    add("flush_partial_after_speech", runs((0.9, 4)), [process(0, 3, 300), {"op": "flush"}])
    add("flush_partial_starts_speech", runs((0.9, 2)), [process(0, 1, 300), {"op": "flush"}])
    add("flush_partial_ends_segment", runs((0.9, 3), (0.02, 3)), [process(0, 3, 300), {"op": "flush"}],
        hangover_frames=0)
    add("flush_not_speaking", runs((0.02, 5)), [process(0, 5), {"op": "flush"}])
    add("flush_keeps_pending_positive", runs((0.9, 1), (0.9, 1), (0.02, 30)),
        [process(0, 1), {"op": "flush"}, process(1, 1), process(2, 30)])
    add("reset_mid_speech", runs((0.9, 6), (0.9, 6), (0.02, 30)), [process(0, 6), {"op": "reset"}, process(6, 36)])
    add("irregular_chunks", runs((0.02, 2), (0.9, 8), (0.02, 30)),
        [process(0, 0, 1000), process(1, 0, 100), process(2, 2), process(4, 0, 1023), process(5, 0, 1), process(6, 40)])
    add("min_speech_zero", runs((0.02, 4), (0.9, 3), (0.02, 30)), [process(0, 37)], min_speech_frames=0)
    add("min_speech_one", runs((0.02, 4), (0.9, 1), (0.02, 30)), [process(0, 35)], min_speech_frames=1)
    add("hangover_zero", runs((0.9, 4), (0.02, 4)), [process(0, 8)], hangover_frames=0)
    add("pad_zero", runs((0.02, 10), (0.9, 6), (0.02, 30)), [process(0, 46)], pre_speech_pad_frames=0)
    add("pad_larger_than_audio", runs((0.02, 3), (0.9, 6), (0.02, 30)), [process(0, 39)], pre_speech_pad_frames=40)
    add("threshold_half_derived_negative", runs((0.5, 4), (float(0.35), 30), (0.3499, 30)), [process(0, 64)],
        speech_threshold=0.5)
    add("threshold_low_negative_zero", runs((0.9, 3), (0.0, 80)), [process(0, 83)],
        speech_threshold=0.1, max_speech_frames=30)
    add("explicit_negative_above_speech", runs((0.9, 3), (0.35, 5), (0.2, 30)), [process(0, 38)],
        speech_threshold=0.3, negative_threshold=0.4)
    return out


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    normalizer = normalizer_cases()
    (HERE / "audio_normalizer_golden.json").write_text(json.dumps(normalizer, indent=1) + "\n", encoding="utf-8")
    vad = scenarios()
    (HERE / "audio_vad_golden.json").write_text(json.dumps(vad, indent=1) + "\n", encoding="utf-8")
    errors = sum(chunk["error"] for case in normalizer["cases"] for chunk in case["chunks"])
    ended = sum(len(step["result"].get("segments", [])) + (1 if step["result"].get("segment") else 0)
                for scenario in vad for step in scenario["steps"])
    print(f"normalizer: {len(normalizer['cases'])} cases ({errors} error chunks); vad: {len(vad)} scenarios, {ended} segments")
    for scenario in vad:
        reasons = [s["reason"] for step in scenario["steps"] for s in step["result"].get("segments", [])]
        reasons += [step["result"]["segment"]["reason"] for step in scenario["steps"] if step["result"].get("segment")]
        print(f"  {scenario['name']}: {reasons}")


if __name__ == "__main__":
    sys.exit(main())
