"""Regenerate `energy_golden.json`: what the energy-threshold recorder of `custom_speech_recognition` does.

VRCT's recorder hands phrase detection to `Recognizer.listen_energy_and_audio_in_background` of the
git-pinned fork `misyaguziya/custom_speech_recognition` (commit afae7ff, tag 3.10.4.5). This runs the REAL
function, unchanged, against scripted audio:

* a fake source whose reads come from a script: chunks of a known waveform, with how far the clock moves
  on each read, how often `get_read_available()` says "nothing yet" first, and the odd empty chunk
  (end of stream) or `OSError`;
* a fake clock (`speech_recognition.time` is replaced), so `record_timeout` and the 10 ms / 100 ms sleeps
  are deterministic;
* when the script runs out the source calls the listener's own stopper, as a user stopping a recording does.

For every scenario it records the phrases the callback received (length and SHA-256 of the audio), every
energy value the `callback_energy` hook saw, the energy threshold after each phrase and at the end, how many
script entries were read and the clock at the end. The Rust port (`transcription::energy`) replays the same
scripts and has to agree on all of it.

The fork is not downloaded by this script and is not part of VRCT's repository: clone it at commit afae7ff
(or copy its `speech_recognition/` folder) to `src-tauri/target/custom_speech_recognition`, or point
`VRCT_SPEECH_RECOGNITION_FORK` at it. Needs Python 3.12 (`audioop`, `aifc`).

Run from anywhere:  python regenerate_energy_golden.py
"""

import hashlib
import json
import math
import os
import random
import sys
import threading
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
FORK = Path(os.environ.get("VRCT_SPEECH_RECOGNITION_FORK", REPO / "src-tauri" / "target" / "custom_speech_recognition"))

sys.path.insert(0, str(FORK))
import audioop  # noqa: E402
import speech_recognition as sr  # noqa: E402

assert Path(sr.__file__).resolve().is_relative_to(FORK.resolve()), f"imported {sr.__file__}, not the fork in {FORK}"
assert sr.__version__ == "3.10.4.5", sr.__version__


# ---- the waveform both sides generate -------------------------------------------------------------

def sample_values(amp: int, samples: int, width: int) -> list:
    """A deterministic waveform of loudness about `amp`, clipped to what `width` bytes hold."""
    limit = (1 << (8 * width - 1)) - 1
    pattern = [amp, -(amp * 3 // 4), amp // 2, -(amp // 3)]
    return [max(-limit - 1, min(limit, pattern[i % 4])) for i in range(samples)]


def pcm(amp: int, samples: int, width: int) -> bytes:
    return b"".join(value.to_bytes(width, "little", signed=True) for value in sample_values(amp, samples, width))


# ---- fakes ----------------------------------------------------------------------------------------

class Clock:
    def __init__(self):
        self.now = 1000.0
        self.sleeps = 0
        self.on_sleep = None

    def time(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds
        if abs(seconds - 0.1) < 1e-12:
            self.sleeps += 1
            if self.on_sleep:
                self.on_sleep(self.sleeps)


class PyAudioStream:
    def __init__(self, source):
        self.source = source

    def get_read_available(self):
        return 1 if self.source.available() else 0


class Stream:
    def __init__(self, source):
        self.source = source
        self.pyaudio_stream = PyAudioStream(source)

    def read(self, count):
        return self.source.read(count)


class Source(sr.AudioSource):
    def __init__(self, scenario, clock):
        p = scenario["params"]
        self.CHUNK = p["chunk"]
        self.SAMPLE_RATE = p["rate"]
        self.SAMPLE_WIDTH = p["width"]
        self.script = scenario["script"]
        self.stop_at = scenario.get("stop_at")
        self.clock = clock
        self.index = 0
        self.polls = 0
        self.stream = None
        self.armed = threading.Event()
        self.done = threading.Event()
        self.stopper = None

    def __enter__(self):
        self.armed.wait()
        self.stream = Stream(self)
        return self

    def __exit__(self, *exc):
        self.stream = None
        self.done.set()

    def available(self):
        if self.index >= len(self.script):
            return True
        if self.polls < self.script[self.index].get("polls_false", 0):
            self.polls += 1
            return False
        return True

    def read(self, count):
        if self.stop_at is not None and self.index == self.stop_at:
            self.stopper(False)
        if self.index >= len(self.script):
            self.stopper(False)
            return b""
        item = self.script[self.index]
        self.index += 1
        self.polls = 0
        self.clock.now += item["advance"]
        if item.get("error"):
            raise OSError("scripted read failure")
        samples = item.get("samples", self.CHUNK)
        return pcm(item["amp"], samples, self.SAMPLE_WIDTH) if samples else b""


# ---- scenarios ------------------------------------------------------------------------------------

def make_scenarios(rng: random.Random, count: int) -> list:
    scenarios = []
    for _ in range(count):
        width = rng.choices([2, 1, 3, 4], [80, 7, 6, 7])[0]
        rate = rng.choice([16000, 16000, 44100, 48000, 16384, 16384])  # 16384: a chunk lasts an exact binary fraction of a second
        chunk = rng.choice([512, 1024, 1024, 1600, 2048])
        top = {1: 100, 2: 20000, 3: 5_000_000, 4: 1_000_000_000}[width]
        scale = {1: 0.005, 2: 1.0, 3: 256.0, 4: 65536.0}[width]
        threshold = rng.choice([0, 50, 300, 300, 1000, 2500.5, 20]) * scale
        pause = rng.choice([0.8, 0.8, 0.3, 1.5])
        params = {
            "chunk": chunk,
            "rate": rate,
            "width": width,
            "energy_threshold": threshold,
            "dynamic": rng.random() < 0.5,
            "damping": rng.choice([0.15, 0.15, 0.5, 0.0]),
            "ratio": rng.choice([1.5, 1.5, 2.0, 1.0]),
            "pause_threshold": pause,
            "phrase_threshold": rng.choice([0.3, 0.3, 0.1, 0.6]),
            "non_speaking_duration": rng.choice([0.5, 0.5, 0.2, 0.0, pause]) if True else 0.5,
            "timeout": rng.choice([1, 1, 1, None, 0.3, 0, 0.5, 2]),
            "phrase_time_limit": rng.choice([None, None, 5, 2, 0.4, 0, 1.0, 0.5]),
            "record_timeout": rng.choice([5, 5, 0.6, None, 1.0, 0.5, 0.25]),
        }
        params["non_speaking_duration"] = min(params["non_speaking_duration"], pause)
        per_chunk = chunk / rate
        fails = rng.random() < 0.12  # most scripts run to their end; some end on a failed read

        script = []
        for _ in range(rng.randint(1, 8)):
            kind = rng.choice(["quiet", "speech", "speech", "mixed"])
            for _ in range(rng.randint(1, 40)):
                loud = kind == "speech" or (kind == "mixed" and rng.random() < 0.5)
                base = max(threshold, 20 * scale)
                amp = int(base * rng.uniform(1.2, 8.0)) if loud else int(base * rng.uniform(0.0, 0.7))
                item = {"amp": min(amp, top), "advance": per_chunk}
                roll = rng.random()
                if roll < 0.04:
                    item["advance"] = rng.choice([0.0, 0.05, 0.4, 1.5])
                elif roll < 0.07:
                    item["samples"] = 0
                elif roll < 0.10:
                    item["samples"] = rng.randint(1, chunk - 1)
                elif roll < 0.12 and fails:
                    item["error"] = True
                if rng.random() < 0.05:
                    item["polls_false"] = rng.randint(1, 3)
                script.append(item)
        scenarios.append(
            {
                "params": params,
                "script": script,
                "pause_at": rng.choice([None, None, None, 1, 2]),
                "resume_after": rng.choice([1, 3]),
                "stop_at": rng.randint(1, len(script)) if rng.random() < 0.3 else None,  # a stop in the middle of the script
            }
        )
    return scenarios


def run(scenario: dict) -> dict:
    p = scenario["params"]
    clock = Clock()
    sr.time = clock
    recognizer = sr.Recognizer()
    recognizer.energy_threshold = p["energy_threshold"]
    recognizer.dynamic_energy_threshold = p["dynamic"]
    recognizer.dynamic_energy_adjustment_damping = p["damping"]
    recognizer.dynamic_energy_ratio = p["ratio"]
    recognizer.pause_threshold = p["pause_threshold"]
    recognizer.phrase_threshold = p["phrase_threshold"]
    recognizer.non_speaking_duration = p["non_speaking_duration"]
    source = Source(scenario, clock)
    phrases, energies = [], []
    controls = {}

    def callback(_, audio):
        data = audio.get_raw_data()
        phrases.append(
            {
                "length": len(data),
                "sha": hashlib.sha256(data).hexdigest(),
                "threshold": recognizer.energy_threshold,
                "clock": clock.now,
                "rate": audio.sample_rate,
                "width": audio.sample_width,
            }
        )
        if scenario["pause_at"] == len(phrases):
            controls["pause"]()

    def on_sleep(count):
        if controls["pause_state"] and count % scenario["resume_after"] == 0:
            controls["resume"]()

    clock.on_sleep = on_sleep
    controls["pause_state"] = False
    record_timeout = float("inf") if p["record_timeout"] is None else p["record_timeout"]
    stop, pause, resume = recognizer.listen_energy_and_audio_in_background(
        source,
        callback,
        phrase_time_limit=p["phrase_time_limit"],
        callback_energy=energies.append,
        phrase_timeout=p["timeout"],
        record_timeout=record_timeout,
    )
    controls["pause"] = lambda: (controls.update(pause_state=True), pause())
    controls["resume"] = lambda: (controls.update(pause_state=False), resume())
    source.stopper = stop
    source.armed.set()
    assert source.done.wait(30), "the listener did not finish"
    return {
        "phrases": phrases,
        "energies": energies,
        "final_threshold": recognizer.energy_threshold,
        "reads": source.index,
        "clock": clock.now,
        "sleeps": clock.sleeps,
    }


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    rng = random.Random(23)
    scenarios = make_scenarios(rng, 300)
    results = []
    for number, scenario in enumerate(scenarios):
        results.append(run(scenario))
    golden = {
        "fork": "misyaguziya/custom_speech_recognition@afae7ff78cd5da36e864122f16972d2b0044e03f",
        "scenarios": [{**scenario, "expected": result} for scenario, result in zip(scenarios, results)],
    }
    (HERE / "energy_golden.json").write_text(json.dumps(golden, separators=(",", ":")), encoding="utf-8")
    phrases = sum(len(r["phrases"]) for r in results)
    print(f"{len(scenarios)} scenarios, {phrases} phrases, {sum(len(r['energies']) for r in results)} energy values")
    print("scenarios with a phrase:", sum(1 for r in results if r["phrases"]), "with a pause:", sum(1 for s in scenarios if s["pause_at"]))


if __name__ == "__main__":
    main()
