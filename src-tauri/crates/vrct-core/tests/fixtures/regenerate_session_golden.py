"""Regenerate `session_golden.json`: how a mic/speaker session starts, runs, fails and stops.

The REAL `_AudioDeviceSession`, `threadFnc` and `_DiscardQueue` are taken out of `src-python/model.py`
(by AST, so their code is exactly VRCT's and `model.py`'s heavy imports are not needed) and run against
scripted fakes: a recorder that records what it is asked, a transcriber that turns queued chunks into
transcripts, and a device list. Each scenario is a list of steps (reconfigure, push audio, wait for N
callbacks...) and the result is one ordered log of everything observable: the calls the recorder got, the
queues it was given, the transcripts / failures / meter values delivered, the log lines, and snapshots
of the session's state. `tests/session.rs` runs the same scenarios against the Rust port.

Chunk text drives the fake transcriber (the same rules are in the Rust test):

    "<raise-pipeline>"  raises AudioPipelineError(ASR_ERROR)   "<raise-generic>"  raises ValueError
    "<batch>"           yields three transcripts in one call   "<hang>"           blocks until released
    "!text"             transcript is flagged as a recognition error afterwards   anything else: a transcript

This reads `src-python` and changes nothing in it. Run from anywhere:  python regenerate_session_golden.py
"""

import ast
import json
import sys
import threading
import time
from pathlib import Path
from queue import Empty, Queue
from threading import Lock, Thread, current_thread
from time import sleep
from typing import Callable, Optional

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
SRC = REPO / "src-python"
sys.path.insert(0, str(SRC))

from errors import ERROR_METADATA, AudioPipelineError, AudioPipelineFailure, ErrorCode  # noqa: E402

EVENTS = []
EVENTS_LOCK = Lock()


def log_event(tag, payload=None):
    with EVENTS_LOCK:
        EVENTS.append([tag, payload])


def count(tag):
    with EVENTS_LOCK:
        return sum(1 for event in EVENTS if event[0] == tag)


# ---- VRCT's own code, lifted out of model.py ------------------------------------------------------------

WANTED = {"_AUDIO_QUEUE_MAXSIZE": "assign", "_DiscardQueue": "class", "threadFnc": "class", "_AudioDeviceSession": "class"}


def lift():
    source = (SRC / "model.py").read_text(encoding="utf-8")
    tree = ast.parse(source)
    pieces = []
    for node in tree.body:
        name = None
        if isinstance(node, ast.ClassDef):
            name = node.name
        elif isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name):
            name = node.targets[0].id
        if name in WANTED:
            pieces.append(ast.get_source_segment(source, node))
    assert len(pieces) == len(WANTED), [p[:30] for p in pieces]
    return "\n\n".join(pieces)


NAMESPACE = {
    "Queue": Queue, "Empty": Empty, "Thread": Thread, "Lock": Lock, "current_thread": current_thread, "sleep": sleep,
    "Optional": Optional, "Callable": Callable, "AudioTranscriber": object,
    "AudioPipelineError": AudioPipelineError, "AudioPipelineFailure": AudioPipelineFailure,
    "ERROR_METADATA": ERROR_METADATA, "ErrorCode": ErrorCode,
    "errorLogging": lambda: None,
    "printLog": lambda text: log_event("log", text),
    "TRANSCRIPT_STOP_JOIN_TIMEOUT": 15,
}
exec(compile(lift(), "model.py(lifted)", "exec"), NAMESPACE)
AudioDeviceSession = NAMESPACE["_AudioDeviceSession"]


# ---- the fakes -------------------------------------------------------------------------------------------

class Context:
    """What a scenario's fakes share."""

    def __init__(self, config):
        self.config = config
        self.selected = {"name": config["selected"], "index": 0} if config.get("selected") else None
        self.gate_closed = False
        self.hang_release = threading.Event()
        self.recorder = None


class FakeRecorder:
    SAMPLE_RATE = 16000
    SAMPLE_WIDTH = 2
    channels = 1

    def __init__(self, ctx):
        self.ctx = ctx
        self.audio_queue = None
        self.energy_queue = None
        self.stop = None
        self.pause = None
        self.resume = None
        self.device_error_event = threading.Event()
        self.device_error_info = None

    def recordIntoQueue(self, audio_queue, energy_queue=None):
        kind = "discard" if isinstance(audio_queue, NAMESPACE["_DiscardQueue"]) else f"bounded:{audio_queue.maxsize}"
        log_event("record_into", {"energy": energy_queue is not None, "queue": kind})
        self.audio_queue, self.energy_queue = audio_queue, energy_queue
        if self.ctx.config.get("fail_record"):
            if self.ctx.config.get("fail_record_info"):
                self.device_error_info = AudioPipelineFailure(
                    ErrorCode.VAD_INFERENCE_ERROR, "vad", "mic", ERROR_METADATA[ErrorCode.VAD_INFERENCE_ERROR]["message"], "RuntimeError")
            raise OSError("cannot record")
        if self.ctx.config.get("info_at_record"):
            # The recorder already knows of a problem, but carries on.
            self.device_error_info = AudioPipelineFailure(
                ErrorCode.VAD_INFERENCE_ERROR, "vad", "mic", ERROR_METADATA[ErrorCode.VAD_INFERENCE_ERROR]["message"], "RuntimeError")
        self.stop = self._stop
        self.pause = self._pause
        self.resume = self._resume

    def _stop(self, wait_for_stop=True):
        log_event("stop")
        hang = self.ctx.config.get("stop_hang_ms", 0)
        if hang:
            time.sleep(hang / 1000)

    def _pause(self):
        log_event("pause")

    def _resume(self):
        log_event("resume")


class FakeTranscriber:
    def __init__(self, ctx):
        self.ctx = ctx
        self.pending = []
        self.last_recognition_error = False

    def transcribeAudioQueue(self, queue, languages, countries, *thresholds):
        if self.ctx.gate_closed:
            time.sleep(0.01)
            return False
        popped = False
        while True:
            try:
                data, _ = queue.get_nowait()
            except Empty:
                break
            popped = True
            text = data.decode()
            if text == "<raise-pipeline>":
                raise AudioPipelineError(AudioPipelineFailure(
                    ErrorCode.ASR_ERROR, "asr", "mic", ERROR_METADATA[ErrorCode.ASR_ERROR]["message"], "RuntimeError"))
            if text == "<raise-generic>":
                raise ValueError("boom")
            if text == "<batch>":
                self.pending += [{"confidence": 0.9, "text": f"b{i}", "language": "en"} for i in (1, 2, 3)]
            elif text == "<hang>":
                self.ctx.hang_release.wait()
            else:
                self.pending.append({"confidence": 0.9, "text": text, "language": "en"})
                self.last_recognition_error = text.startswith("!")
        if not popped:
            time.sleep(0.01)
        return popped

    def hasTranscript(self):
        return len(self.pending) > 0

    def getTranscript(self):
        return self.pending.pop(0)


class FakeSession(AudioDeviceSession):
    _kind = "mic"

    def __init__(self, ctx):
        super().__init__()
        self.ctx = ctx

    def _resolve_device(self, override=None):
        if override is not None:
            if override.get("name") == "NoDevice":
                return None
            return override
        return self.ctx.selected

    def _create_recorder(self, device):
        if self.ctx.config.get("fail_open"):
            log_event("open_failed", device["name"])
            raise OSError("cannot open")
        log_event("open", device["name"])
        self.ctx.recorder = FakeRecorder(self.ctx)
        return self.ctx.recorder

    def _create_transcriber(self):
        if self.ctx.config.get("fail_transcriber"):
            log_event("transcriber_failed")
            raise RuntimeError("no engine")
        log_event("transcriber", {"sample_rate": self._recorder.SAMPLE_RATE, "channels": self._recorder.channels})
        return FakeTranscriber(self.ctx)

    def _transcribe(self, transcriber, queue):
        return transcriber.transcribeAudioQueue(queue, ["en"], ["US"], -0.8, 0.6, 0)


# ---- the scenarios ---------------------------------------------------------------------------------------

def device(name):
    return {"name": name, "index": 0}


def wait_for(tag, wanted, timeout_ms=5000):
    deadline = time.time() + timeout_ms / 1000
    while count(tag) < wanted:
        if time.time() > deadline:
            raise TimeoutError(f"waiting for {wanted} x {tag}")
        time.sleep(0.005)


def run_step(session, ctx, step):
    op = step["op"]
    if op == "reconfigure":
        override = device(step["device"]) if step.get("device") else None
        try:
            session.reconfigure(transcript=step.get("transcript"), energy=step.get("energy"), device=override)
        except Exception:
            log_event("raised")
    elif op == "pause":
        session.pause()
    elif op == "resume":
        session.resume()
    elif op == "select":
        ctx.selected = device(step["name"]) if step.get("name") else None
    elif op == "audio":
        for text in step["chunks"]:
            put_dropping_oldest(ctx.recorder.audio_queue, (text.encode(), None))
    elif op == "energy":
        for value in step["values"]:
            put_dropping_oldest(ctx.recorder.energy_queue, value)
    elif op == "device_error":
        ctx.recorder.device_error_info = AudioPipelineFailure(
            ErrorCode.AUDIO_READ_ERROR, "recording", "mic", ERROR_METADATA[ErrorCode.AUDIO_READ_ERROR]["message"], "OSError")
        ctx.recorder.device_error_event.set()
    elif op == "gate":
        ctx.gate_closed = step["closed"]
    elif op == "release":
        ctx.hang_release.set()
    elif op == "wait_for":
        wait_for(step["tag"], step["count"])
    elif op == "sleep":
        time.sleep(step["ms"] / 1000)
    elif op == "snapshot":
        log_event("snapshot", {
            "transcript": "transcript" in session.features,
            "energy": "energy" in session.features,
            "device": (session._active_device or {}).get("name"),
        })
    else:
        raise ValueError(op)


def put_dropping_oldest(queue, item):
    """What the recorder's callback does: putDroppingOldestOnFull (utils.py)."""
    import queue as queue_module
    try:
        queue.put_nowait(item)
    except queue_module.Full:
        try:
            queue.get_nowait()
        except queue_module.Empty:
            pass
        try:
            queue.put_nowait(item)
        except queue_module.Full:
            pass


def run_scenario(scenario):
    with EVENTS_LOCK:
        EVENTS.clear()
    ctx = Context(scenario["config"])
    NAMESPACE["TRANSCRIPT_STOP_JOIN_TIMEOUT"] = scenario["config"].get("stop_timeout_ms", 15000) / 1000
    session = FakeSession(ctx)

    def on_transcript(result):
        log_event("deliver", result)

    def on_energy(value):
        log_event("level", value)

    session.transcript_fnc = on_transcript
    session.energy_fnc = on_energy
    for step in scenario["steps"]:
        run_step(session, ctx, step)
    ctx.hang_release.set()
    with EVENTS_LOCK:
        return [list(event) for event in EVENTS]


def S(name, config, *steps):
    return {"name": name, "config": {"selected": "Mic A", **config}, "steps": list(steps)}


def on(**kw):
    return {"op": "reconfigure", **kw}


def audio(*chunks):
    return {"op": "audio", "chunks": list(chunks)}


def wait(tag, n):
    return {"op": "wait_for", "tag": tag, "count": n}


SNAP = {"op": "snapshot"}
OFF = on(transcript=False, energy=False)

SCENARIOS = [
    S("transcript_basic", {}, on(transcript=True), SNAP, audio("hello", "world"), wait("deliver", 2), SNAP, on(transcript=False), SNAP),
    S("energy_only", {}, on(energy=True), SNAP,
      {"op": "energy", "values": [5]}, wait("level", 1), {"op": "energy", "values": [9]}, wait("level", 2), SNAP, on(energy=False), SNAP,
      {"op": "energy", "values": [7]}, {"op": "sleep", "ms": 80}),
    S("both_features_restart", {}, on(transcript=True), audio("a"), wait("deliver", 1), on(energy=True), SNAP,
      audio("b"), wait("deliver", 2), {"op": "energy", "values": [3]}, wait("level", 1), SNAP, on(transcript=False), SNAP, OFF, SNAP),
    S("same_state_is_a_noop", {}, on(transcript=True), on(transcript=True), SNAP, on(energy=False), SNAP, on(transcript=False)),
    S("no_device_override", {}, on(transcript=True, device="NoDevice"), SNAP, on(energy=True, device="NoDevice"), SNAP),
    S("no_device_selected", {"selected": None}, on(transcript=True), SNAP, on(energy=True), SNAP),
    S("device_change_restarts", {}, on(transcript=True), SNAP, on(device="Mic B"), SNAP, on(device="Mic B"), audio("x"), wait("deliver", 1),
      {"op": "select", "name": "Mic C"}, on(), SNAP, on(transcript=False)),
    S("open_fails", {"fail_open": True}, on(transcript=True), SNAP),
    S("record_fails", {"fail_record": True}, on(transcript=True), SNAP),
    S("record_fails_with_recorder_info", {"fail_record": True, "fail_record_info": True}, on(transcript=True), SNAP),
    S("transcriber_init_fails", {"fail_transcriber": True}, on(transcript=True), SNAP),
    S("cleanup_timeout_on_a_failed_start", {"fail_transcriber": True, "stop_hang_ms": 1500, "stop_timeout_ms": 300}, on(transcript=True), SNAP),
    S("start_failure_beats_recorder_info", {"fail_transcriber": True, "info_at_record": True}, on(transcript=True), SNAP),
    S("asr_pipeline_error", {}, on(transcript=True), audio("ok"), wait("deliver", 1), audio("<raise-pipeline>"), wait("deliver", 2),
      {"op": "sleep", "ms": 50}, SNAP, audio("late"), {"op": "sleep", "ms": 50}),
    S("asr_generic_error", {}, on(transcript=True), audio("<raise-generic>"), wait("deliver", 1), {"op": "sleep", "ms": 50}, SNAP),
    S("device_error_is_reported", {}, on(transcript=True), {"op": "device_error"}, wait("deliver", 1),
      {"op": "sleep", "ms": 50}, SNAP),
    S("restart_after_failure", {}, on(transcript=True), audio("<raise-pipeline>"), wait("deliver", 1), {"op": "sleep", "ms": 50}, SNAP,
      on(transcript=True), SNAP, audio("again"), wait("deliver", 2), audio("<raise-pipeline>"), wait("deliver", 3), {"op": "sleep", "ms": 50}, SNAP),
    S("batch_delivers_all", {}, on(transcript=True), audio("<batch>"), wait("deliver", 3), on(transcript=False)),
    S("recognition_error_flag", {}, on(transcript=True), audio("!bad"), wait("deliver", 1), audio("good"), wait("deliver", 2), on(transcript=False)),
    S("queue_overflow_drops_oldest", {}, on(transcript=True), {"op": "gate", "closed": True},
      audio(*[f"c{i:02d}" for i in range(25)]), {"op": "gate", "closed": False}, wait("deliver", 20), {"op": "sleep", "ms": 60}, on(transcript=False)),
    S("resume_drains_the_queue", {}, on(transcript=True), {"op": "gate", "closed": True}, audio("a", "b"), {"op": "pause"}, {"op": "resume"},
      {"op": "gate", "closed": False}, audio("c"), wait("deliver", 1), {"op": "sleep", "ms": 60}, on(transcript=False)),
    S("stop_while_paused", {}, on(transcript=True), {"op": "pause"}, on(transcript=False), SNAP),
    S("cleanup_timeout_replaces_the_error", {"stop_hang_ms": 1500, "stop_timeout_ms": 300},
      on(transcript=True), audio("<raise-pipeline>"), wait("deliver", 1), SNAP),
    S("hung_transcriber_times_out", {"stop_timeout_ms": 300},
      on(transcript=True), audio("<hang>"), {"op": "sleep", "ms": 60}, on(transcript=False), SNAP, {"op": "release"}),
    S("hung_recorder_stop_times_out", {"stop_hang_ms": 1500, "stop_timeout_ms": 300},
      on(transcript=True), on(transcript=False), SNAP),
]


def main():
    results = []
    for scenario in SCENARIOS:
        events = run_scenario(scenario)
        results.append({**scenario, "events": events})
        print(f"{scenario['name']}: {len(events)} events")
    out = HERE / "session_golden.json"
    out.write_text(json.dumps({"python": sys.version.split()[0], "scenarios": results}, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
