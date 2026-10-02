"""Regenerate silero_golden.json from the repo's own `SileroFrameProbability`.

`SileroFrameProbability` asks `faster_whisper.vad.get_vad_model()` for a model
with an `encoder_session` and a `decoder_session`. That is faster-whisper 1.1.1
(the version `requirements.txt` pins); the 1.2.x installed on a development
machine ships a different (v6) single network. So the model object of 1.1.1 is
rebuilt here around the same two ONNX files (the ones committed in
`assets/silero/`, taken from the 1.1.1 wheel, same session options) and handed
to the repo class, which then runs unchanged.

The audio is synthetic (nothing recorded, nothing downloaded): silence, noise at
three levels, and a voiced "vowel" (harmonics of a gliding pitch through formant
resonators) with a few pauses. Samples are stored as base64 int16 so Rust reads
the very same input.

Run from anywhere:  python regenerate_silero_golden.py
"""

import base64
import hashlib
import json
import sys
import warnings
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "src-python"))

import numpy as np  # noqa: E402
import onnxruntime  # noqa: E402

warnings.simplefilter("ignore", DeprecationWarning)

import faster_whisper.vad as fw_vad  # noqa: E402
from models.transcription.audio_vad import FRAME_SAMPLES, SileroFrameProbability, VadSegmenter  # noqa: E402

HERE = Path(__file__).resolve().parent
ASSETS = HERE.parent.parent / "assets" / "silero"
RATE = 16000


class SileroVADModel:
    """faster-whisper 1.1.1's class, as it builds its sessions."""

    def __init__(self, encoder_path, decoder_path):
        opts = onnxruntime.SessionOptions()
        opts.inter_op_num_threads = 1
        opts.intra_op_num_threads = 1
        opts.enable_cpu_mem_arena = False
        opts.log_severity_level = 4
        self.encoder_session = onnxruntime.InferenceSession(encoder_path, providers=["CPUExecutionProvider"], sess_options=opts)
        self.decoder_session = onnxruntime.InferenceSession(decoder_path, providers=["CPUExecutionProvider"], sess_options=opts)


MODEL = SileroVADModel(str(ASSETS / "silero_encoder_v5.onnx"), str(ASSETS / "silero_decoder_v5.onnx"))
fw_vad.get_vad_model = lambda: MODEL


def lcg_noise(count: int, amplitude: int, seed: int) -> np.ndarray:
    state = seed
    out = np.empty(count, dtype=np.int64)
    for i in range(count):
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        out[i] = ((state >> 8) % (2 * amplitude + 1)) - amplitude
    return out


def resonator(signal: np.ndarray, freq: float, bandwidth: float) -> np.ndarray:
    r = np.exp(-np.pi * bandwidth / RATE)
    a1, a2 = 2 * r * np.cos(2 * np.pi * freq / RATE), -r * r
    out = np.zeros_like(signal)
    y1 = y2 = 0.0
    for i, x in enumerate(signal):
        y = x + a1 * y1 + a2 * y2
        out[i] = y
        y2, y1 = y1, y
    return out


def vowel(seconds: float, formants, pitch_start: float, pitch_end: float, pauses) -> np.ndarray:
    count = int(seconds * RATE)
    t = np.arange(count) / RATE
    pitch = np.linspace(pitch_start, pitch_end, count)
    phase = 2 * np.pi * np.cumsum(pitch) / RATE
    source = np.zeros(count)
    for k in range(1, 30):
        source += np.sin(k * phase) / k
    source *= 1.0 + 0.04 * np.sin(2 * np.pi * 5.5 * t)
    voiced = sum(resonator(source, f, b) for f, b in formants)
    envelope = np.ones(count)
    for start, stop in pauses:
        envelope[int(start * RATE):int(stop * RATE)] = 0.0
    ramp = int(0.01 * RATE)
    envelope = np.convolve(envelope, np.ones(ramp) / ramp, mode="same")
    signal = voiced * envelope
    signal = signal / np.max(np.abs(signal)) * 14000
    return signal + lcg_noise(count, 40, 7)


def to_int16(signal: np.ndarray) -> np.ndarray:
    return np.clip(np.rint(signal), -32768, 32767).astype("<i2")


def frames_of(signal: np.ndarray) -> list:
    samples = to_int16(signal)
    usable = len(samples) - len(samples) % FRAME_SAMPLES
    return [samples[i:i + FRAME_SAMPLES] for i in range(0, usable, FRAME_SAMPLES)]


def run(ops: list) -> dict:
    """ops: arrays of one frame, or the string "reset"."""
    engine = SileroFrameProbability()
    out = []
    for op in ops:
        if isinstance(op, str):
            engine.reset()
            continue
        out.append(engine(op.astype(np.float32) / 32768.0))
    return out


def scenario(name: str, ops: list) -> dict:
    probs = run(ops)
    frames = [op for op in ops if not isinstance(op, str)]
    encoded = [{"reset": True} if isinstance(op, str) else {"pcm": base64.b64encode(op.tobytes()).decode()} for op in ops]
    return {"name": name, "ops": encoded, "probs": probs, "frames": len(frames)}


def pipeline(frames: list) -> dict:
    """The whole chain: `VadSegmenter` (default settings) around the real engine, one `process` call per chunk."""
    segmenter = VadSegmenter(SileroFrameProbability())
    base = segmenter._segment_id
    segments = []
    pcm = b"".join(frame.tobytes() for frame in frames)
    chunk = FRAME_SAMPLES * 2 * 3 + 100  # not a multiple of the frame size
    for start in range(0, len(pcm), chunk):
        segments += segmenter.process(pcm[start:start + chunk])
    tail = segmenter.flush()
    if tail is not None:
        segments.append(tail)
    return {
        "pcm": base64.b64encode(pcm).decode(), "chunk_bytes": chunk,
        "segments": [{"len": len(g.audio), "sha256": hashlib.sha256(g.audio).hexdigest(), "id": g.segment_id - base,
                      "reason": g.reason} for g in segments],
    }


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    speech_a = frames_of(vowel(1.6, [(700, 110), (1200, 120), (2600, 160)], 120, 150, [(0.55, 0.8)]))
    speech_b = frames_of(vowel(1.2, [(300, 90), (2300, 140), (3000, 200)], 190, 170, [(0.4, 0.6)]))
    zeros = frames_of(np.zeros(FRAME_SAMPLES * 8))
    quiet = frames_of(lcg_noise(FRAME_SAMPLES * 12, 100, 11))
    mid = frames_of(lcg_noise(FRAME_SAMPLES * 12, 3000, 12))
    loud = frames_of(lcg_noise(FRAME_SAMPLES * 12, 20000, 13))
    scenarios = [
        scenario("silence", zeros),
        scenario("noise_quiet", quiet),
        scenario("noise_mid", mid),
        scenario("noise_loud", loud),
        scenario("vowel_a", speech_a),
        scenario("vowel_b", speech_b),
        # State and context carry across frames; a reset puts both back, so the
        # replayed frames must give the first run's numbers again.
        scenario("reset_replays", speech_a[:12] + ["reset"] + speech_a[:12] + speech_b[:6] + ["reset"] + zeros[:3]),
    ]
    silence = frames_of(np.zeros(FRAME_SAMPLES * 40))
    whole = pipeline(zeros + speech_a + silence + speech_b + silence)
    document = {"scenarios": scenarios, "pipeline": whole}
    (HERE / "silero_golden.json").write_text(json.dumps(document, indent=1) + "\n", encoding="utf-8")
    print("pipeline segments:", [(g["reason"], g["len"]) for g in whole["segments"]])
    for s in scenarios:
        probs = s["probs"]
        print(f"{s['name']}: {s['frames']} frames, min {min(probs):.4f} max {max(probs):.4f} "
              f">=0.25: {sum(p >= 0.25 for p in probs)}")


if __name__ == "__main__":
    sys.exit(main())
