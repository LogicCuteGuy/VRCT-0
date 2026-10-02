"""Regenerate `phrases_golden.json` from the real `AudioTranscriber.transcribeAudioQueue`.

What is recorded is the phrase accumulation: which queued chunks are joined, when the joined audio is
sent for recognition (and with what padding), what the recogniser is asked, and what is left in the
transcript list and the counters afterwards. The recogniser itself is a scripted stand-in: every
call it receives is recorded, and it answers from a list that the Rust test replays.

Stubbed out (not installed here, and not what is being checked): `speech_recognition`,
`pyaudiowpatch`, `pydub`, and the Whisper / cloud provider modules. The wall clock (`datetime.now`)
and `time.sleep` inside the transcriber module are replaced by a scripted clock.

Chunk audio is a formula, `byte i = (seed + 7 * i) % 251`, so the file stores only lengths and seeds.

Run from anywhere:  python regenerate_phrases_golden.py
"""

import hashlib
import json
import random
import sys
import types
from datetime import datetime, timedelta
from pathlib import Path
from queue import Queue

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "src-python"))
HERE = Path(__file__).resolve().parent

# ---- stand-ins for what is not installed ---------------------------------------------------------


class UnknownValueError(Exception):
    pass


class TranscriptionApiError(Exception):
    def __init__(self, error_code):
        super().__init__(str(error_code))
        self.error_code = error_code


class AudioData:
    def __init__(self, frame_data, sample_rate, sample_width):
        self.frame_data = frame_data
        self.sample_rate = sample_rate
        self.sample_width = sample_width


def stub(name, **attrs):
    module = types.ModuleType(name)
    module.__dict__.update(attrs)
    sys.modules[name] = module
    return module


class Recognizer:
    operation_timeout = None


stub("speech_recognition", Recognizer=Recognizer, AudioData=AudioData, AudioFile=object)
stub("speech_recognition.exceptions", UnknownValueError=UnknownValueError)
stub("pyaudiowpatch", get_sample_size=lambda fmt: 2, paInt16=8)
stub("pydub", AudioSegment=object)
stub("models.transcription.transcription_whisper", getWhisperModel=lambda *a, **k: None, checkWhisperWeight=lambda *a, **k: False)
stub("models.transcription.transcription_openai_compatible", TRANSCRIPTION_API_ENGINES=("Groq_Whisper", "OpenAI_Whisper", "Custom_Whisper"))
stub(
    "models.transcription.transcription_providers",
    GoogleProvider=object,
    LocalWhisperProvider=object,
    OpenAICompatibleTranscriptionProvider=object,
    DeepgramProvider=object,
    TranscriptionApiError=TranscriptionApiError,
)

import models.transcription.transcription_transcriber as tt  # noqa: E402
from errors import AudioPipelineError, ErrorCode  # noqa: E402

BASE = datetime(2026, 1, 1, 12, 0, 0)


class Clock:
    now_value = BASE


class FakeDatetime(datetime):
    @classmethod
    def now(cls, tz=None):
        return Clock.now_value


tt.datetime = FakeDatetime
tt.time = types.SimpleNamespace(sleep=lambda s: None, perf_counter=lambda: 0.0)


def audio_bytes(length: int, seed: int) -> bytes:
    period = bytes((seed + 7 * i) % 251 for i in range(251))
    return (period * (length // 251 + 1))[:length]


def ms(moment) -> "int | None":
    return None if moment is None else round((moment - BASE) / timedelta(milliseconds=1))


class Exhausted(BaseException):
    """The scenario asked the recogniser more often than it scripted replies (not catchable as Exception)."""


class Provider:
    """Answers from `script` in order, and records what it was asked."""

    def __init__(self, script):
        self.script = list(script)
        self.calls = []

    def transcribe(self, audio, language, country, **kwargs):
        data = audio.frame_data
        self.calls.append(
            {
                "language": language,
                "country": country,
                "len": len(data),
                "sha": hashlib.sha256(data).hexdigest(),
                "kwargs": kwargs,
            }
        )
        if not self.script:
            raise Exhausted()
        reply = self.script.pop(0)
        if "unknown" in reply:
            raise UnknownValueError()
        if "api_error" in reply:
            raise TranscriptionApiError(ErrorCode(reply["api_error"]))
        if "error" in reply:
            raise RuntimeError(reply["error"])
        return reply["text"], reply["confidence"], reply["definitive"]


def run_scenario(scenario, strict=True):
    cfg = scenario["config"]
    source = types.SimpleNamespace(SAMPLE_RATE=cfg["sample_rate"], SAMPLE_WIDTH=cfg["sample_width"], channels=cfg["channels"])
    transcriber = tt.AudioTranscriber(
        speaker=cfg["speaker"],
        source=source,
        phrase_timeout=cfg["phrase_timeout"],
        max_phrases=cfg["max_phrases"],
        transcription_engine="Google",
        vad_segmented=cfg["vad_segmented"],
    )
    transcriber.transcription_engine = cfg["engine"]
    provider = Provider(scenario["replies"])
    transcriber._resolve_provider = lambda: provider
    transcriber.audio_sources["process_data_func"] = transcriber.processMicData
    queue = Queue()
    steps = []
    for step in scenario["steps"]:
        for chunk in step["chunks"]:
            data = audio_bytes(chunk["len"], chunk["seed"])
            at = BASE + timedelta(milliseconds=chunk["at_ms"])
            queue.put((data, at, chunk["reason"]) if cfg["vad_segmented"] else (data, at))
        Clock.now_value = BASE + timedelta(milliseconds=step["now_ms"])
        before = len(provider.calls)
        record = {}
        try:
            record["result"] = transcriber.transcribeAudioQueue(
                queue,
                step["languages"],
                step["countries"],
                step["avg_logprob"],
                step["no_speech_prob"],
                step["no_repeat_ngram_size"],
            )
            record["error"] = None
        except AudioPipelineError as error:
            record["result"] = None
            record["error"] = {
                "code": error.failure.error_code.value,
                "stage": error.failure.stage,
                "source": error.failure.source,
                "exception_type": error.failure.exception_type,
            }
        record["calls"] = provider.calls[before:]
        source_info = transcriber.audio_sources
        record["state"] = {
            "last_sample_len": len(source_info["last_sample"]),
            "last_spoken_ms": ms(source_info["last_spoken"]),
            "phrase_started_ms": ms(source_info["phrase_started_at"]),
            "queue_left": queue.qsize(),
            "transcripts": [
                {"text": t["text"], "confidence": t["confidence"], "language": t["language"]}
                for t in transcriber.transcript_data
            ],
            "last_recognition_error": transcriber.last_recognition_error,
            "last_api_error_code": None if transcriber.last_api_error_code is None else transcriber.last_api_error_code.value,
            "asr_attempts": transcriber.asr_attempts,
            "asr_successes": transcriber.asr_successes,
        }
        record["drained"] = []
        if step.get("drain"):
            while transcriber.hasTranscript():
                got = transcriber.getTranscript()
                record["drained"].append({"text": got["text"], "confidence": got["confidence"], "language": got["language"]})
            record["drained"].append({"empty": transcriber.getTranscript()["text"]})
        if step.get("clear"):
            transcriber.clearTranscriptData()
            record["after_clear"] = {
                "last_sample_len": len(source_info["last_sample"]),
                "last_spoken_ms": ms(source_info["last_spoken"]),
                "phrase_started_ms": ms(source_info["phrase_started_at"]),
                "transcripts": len(transcriber.transcript_data),
            }
        steps.append(record)
    used = len(scenario["replies"]) - len(provider.script)
    if strict:
        assert not provider.script, "scripted replies left over: the scenario asks for more than was used"
    return {"config": cfg, "steps": steps, "used": used}


# ---- scenarios -------------------------------------------------------------------------------------

REASONS = ["silence", "flush", "max_duration", None]
ERROR_CODES = [
    "TRANSCRIPTION_API_AUTH_FAILED",
    "TRANSCRIPTION_API_RATE_LIMITED",
    "TRANSCRIPTION_API_TIMEOUT",
    "TRANSCRIPTION_API_SERVER_ERROR",
]


def chunk(len_, at_ms, reason=None, seed=1):
    return {"len": len_, "at_ms": at_ms, "reason": reason, "seed": seed}


def step(chunks, now_ms, languages=("en",), countries=("US",), **kw):
    return {
        "chunks": chunks,
        "now_ms": now_ms,
        "languages": list(languages),
        "countries": list(countries),
        "avg_logprob": kw.get("avg_logprob", -0.8),
        "no_speech_prob": kw.get("no_speech_prob", 0.6),
        "no_repeat_ngram_size": kw.get("no_repeat_ngram_size", 0),
        "drain": kw.get("drain", False),
        "clear": kw.get("clear", False),
    }


def config(engine="Google", vad=False, speaker=False, rate=16000, width=2, channels=1, phrase_timeout=3, max_phrases=10):
    return {
        "engine": engine,
        "vad_segmented": vad,
        "speaker": speaker,
        "sample_rate": rate,
        "sample_width": width,
        "channels": channels,
        "phrase_timeout": phrase_timeout,
        "max_phrases": max_phrases,
    }


def ok(text, confidence=0.9, definitive=False):
    return {"text": text, "confidence": confidence, "definitive": definitive}


def fixed_scenarios():
    one_second = 32000  # 16 kHz, 16 bit, mono
    out = []

    def add(name, cfg, replies, steps):
        out.append({"name": name, "config": cfg, "replies": replies, "steps": steps})

    # energy-threshold path
    add("energy whisper: two chunks within the timeout stay buffered, then the quiet gap sends",
        config("Whisper"), [ok("hello", 0.8, True)],
        [step([chunk(one_second, 0), chunk(one_second, 1000)], 2000), step([], 6000, drain=True)])
    add("energy whisper: a gap longer than phrase_timeout between chunks finalizes the earlier one",
        config("Whisper"), [ok("first", 0.7, True), ok("second", 0.7, True)],
        [step([chunk(one_second, 0), chunk(one_second, 5000)], 5100), step([], 9000, drain=True)])
    add("energy google: interim sends, one per drain",
        config("Google"), [ok("he", 0.5), ok("hello", 0.6), ok("hello world", 0.7)],
        [step([chunk(one_second, 0)], 100), step([chunk(one_second, 1000)], 1100), step([chunk(one_second, 2000)], 2100),
         step([], 3000), step([], 9000, drain=True)])
    add("energy google: a backlog is folded into one send",
        config("Google"), [ok("all of it", 0.8)],
        [step([chunk(one_second, 0), chunk(one_second, 500), chunk(one_second, 900)], 1000, drain=True)])
    add("energy google: gap after an interim send only resets",
        config("Google"), [ok("a", 0.4), ok("b", 0.4)],
        [step([chunk(one_second, 0)], 100), step([chunk(one_second, 6000)], 6100, drain=True)])
    add("energy google: gap with a folded (unsent) backlog sends before reset",
        config("Google"), [ok("a", 0.4), ok("b", 0.4)],
        [step([chunk(one_second, 0)], 100), step([chunk(one_second, 6000), chunk(one_second, 6100)], 6200, drain=True)])
    add("energy: the 15 s safety valve",
        config("Whisper"), [ok("long", 0.9, True)],
        [step([chunk(one_second * 8, 0), chunk(one_second * 8, 1000)], 1100, drain=True)])
    add("energy: exactly 15 s buffered",
        config("Whisper"), [ok("edge", 0.9, True)],
        [step([chunk(one_second * 15, 0)], 100, drain=True)])
    add("energy: just under 15 s buffered",
        config("Whisper"), [ok("edge", 0.9, True)],
        [step([chunk(one_second * 15 - 2, 0)], 100), step([], 5000, drain=True)])
    add("energy: timeout exactly equal does not finalize",
        config("Whisper"), [ok("same", 0.9, True)],
        [step([chunk(one_second, 0), chunk(one_second, 3000)], 3000), step([], 6001, drain=True)])
    add("energy: real-time timeout after the queue empties (strict)",
        config("Whisper"), [ok("late", 0.9, True)],
        [step([chunk(one_second, 0)], 3000), step([], 3001, drain=True)])
    add("energy: nothing queued",
        config("Whisper"), [], [step([], 0, drain=True)])
    add("energy: empty chunks only",
        config("Whisper"), [], [step([chunk(0, 0)], 100), step([], 5000, drain=True)])
    add("energy: returns after the first transcript, leaving the queue",
        config("Whisper"), [ok("one", 0.9, True), ok("two", 0.9, True), ok("three", 0.9, True)],
        [step([chunk(one_second, 0), chunk(one_second, 5000), chunk(one_second, 10000)], 10100),
         step([], 10200), step([], 20000, drain=True)])
    add("energy: unmatched language, then a match",
        config("Whisper"), [{"unknown": True}, ok("hola", 0.6, False)],
        [step([chunk(one_second, 0)], 100, languages=["en", "es"], countries=["US", "ES"]), step([], 5000, languages=["en", "es"], countries=["US", "ES"], drain=True)])
    add("energy: best of several languages, definitive stops early",
        config("Whisper"), [ok("a", 0.2, False), ok("b", 0.9, True)],
        [step([chunk(one_second, 0)], 100, languages=["en", "es", "ja"], countries=["US", "ES", "JP"]),
         step([], 5000, languages=["en", "es", "ja"], countries=["US", "ES", "JP"], drain=True)])
    add("energy: equal confidence keeps the first",
        config("Whisper"), [ok("first", 0.5, False), ok("second", 0.5, False)],
        [step([chunk(one_second, 0)], 100, languages=["en", "es"], countries=["US", "ES"]),
         step([], 5000, languages=["en", "es"], countries=["US", "ES"], drain=True)])
    add("energy: zero confidence never wins",
        config("Whisper"), [ok("silent", 0.0, True)],
        [step([chunk(one_second, 0)], 100), step([], 5000, drain=True)])
    add("energy: mismatched language and country lists",
        config("Whisper"), [ok("x", 0.5, False)],
        [step([chunk(one_second, 0)], 100, languages=["en", "es"], countries=["US"]),
         step([], 5000, languages=["en", "es"], countries=["US"], drain=True)])
    add("energy: a thresholds pass through to the recogniser",
        config("Whisper"), [ok("t", 0.5, True)],
        [step([chunk(one_second, 0)], 100, avg_logprob=-0.3, no_speech_prob=0.9, no_repeat_ngram_size=3),
         step([], 5000, avg_logprob=-0.3, no_speech_prob=0.9, no_repeat_ngram_size=3, drain=True)])
    add("energy: max_phrases bounds the transcript list",
        config("Whisper", max_phrases=2),
        [ok(f"p{i}", 0.9, True) for i in range(6)],
        [step([chunk(one_second, i * 10000)], i * 10000 + 100) for i in range(6)] + [step([], 100000, drain=True)])
    add("energy: max_phrases zero",
        config("Whisper", max_phrases=0),
        [ok(f"p{i}", 0.9, True) for i in range(3)],
        [step([chunk(one_second, i * 10000)], i * 10000 + 100) for i in range(3)] + [step([], 100000, drain=True)])
    add("energy: failure with only errors raises, and counts",
        config("Whisper"), [{"error": "boom"}, ok("later", 0.5, True)],
        [step([chunk(one_second, 0)], 100), step([], 5000), step([], 9000, drain=True)])
    add("energy google: api errors set the error code",
        config("Google"), [{"api_error": "TRANSCRIPTION_API_AUTH_FAILED"}],
        [step([chunk(one_second, 0)], 100), step([], 9000, drain=True)])
    add("energy cloud: api error on one language, text on another",
        config("OpenAI_Whisper"), [{"api_error": "TRANSCRIPTION_API_TIMEOUT"}, ok("ok", 0.5, False), ok("ok2", 0.5, False), ok("ok3", 0.6, False)],
        [step([chunk(one_second, 0)], 100, languages=["en", "es"], countries=["US", "ES"]),
         step([chunk(one_second, 6000)], 6100, languages=["en", "es"], countries=["US", "ES"]),
         step([], 20000, languages=["en", "es"], countries=["US", "ES"], drain=True)])
    add("energy cloud: error then clean success resets the flag",
        config("Groq_Whisper"), [{"error": "x"}, ok("fine", 0.5, True), ok("again", 0.5, True)],
        [step([chunk(one_second, 0)], 100), step([], 5000), step([chunk(one_second, 20000)], 20100), step([], 30000, drain=True)])
    add("energy deepgram: counts as cloud",
        config("Deepgram"), [{"api_error": "TRANSCRIPTION_API_SERVER_ERROR"}, ok("dg", 0.5, True), ok("dg2", 0.5, True)],
        [step([chunk(one_second, 0)], 100), step([], 5000), step([chunk(one_second, 20000)], 20100), step([], 30000, drain=True)])
    add("energy whisper: errors are not cleared by the next call (local Whisper keeps the flag)",
        config("Whisper"), [{"error": "x"}, {"unknown": True}, {"unknown": True}],
        [step([chunk(one_second, 0)], 100), step([], 5000), step([chunk(one_second, 20000)], 20100), step([], 30000)])
    add("energy: clear drops the buffered audio unsent",
        config("Whisper"), [],
        [step([chunk(one_second, 0)], 100, clear=True), step([], 9000, drain=True)])
    add("energy: other sample formats and speaker flag",
        config("Whisper", speaker=True, rate=48000, width=2, channels=2), [ok("s", 0.5, True)],
        [step([chunk(192000, 0)], 100), step([], 9000, drain=True)])
    add("energy: fractional phrase_timeout path",
        config("Whisper", phrase_timeout=0), [ok("a", 0.5, True), ok("b", 0.5, True)],
        [step([chunk(one_second, 0), chunk(one_second, 1)], 5), step([], 100, drain=True)])

    # VAD path
    add("vad: a natural boundary finalizes at once",
        config("Whisper", vad=True), [ok("clip", 0.9, True)],
        [step([chunk(one_second, 0, "silence")], 100, drain=True)])
    add("vad: max_duration accumulates until a natural boundary",
        config("Whisper", vad=True), [ok("merged", 0.9, True)],
        [step([chunk(one_second, 0, "max_duration")], 100), step([chunk(one_second, 1000, "max_duration")], 1100),
         step([chunk(one_second, 2000, "flush")], 2100, drain=True)])
    add("vad: reason none counts as natural",
        config("Whisper", vad=True), [ok("n", 0.9, True)],
        [step([chunk(one_second, 0, None)], 100, drain=True)])
    add("vad: safety valve at 15 s of accumulated audio",
        config("Whisper", vad=True), [ok("valve", 0.9, True)],
        [step([chunk(one_second * 7, 0, "max_duration"), chunk(one_second * 7, 100, "max_duration"), chunk(one_second * 2, 200, "max_duration")], 300, drain=True)])
    add("vad: just under the valve",
        config("Whisper", vad=True), [ok("end", 0.9, True)],
        [step([chunk(one_second * 15 - 2, 0, "max_duration")], 100), step([chunk(2, 200, "silence")], 300, drain=True)])
    add("vad google: interim send for max_duration when the queue is empty",
        config("Google", vad=True), [ok("i1", 0.3), ok("i2", 0.4), ok("final", 0.6)],
        [step([chunk(one_second, 0, "max_duration")], 100), step([chunk(one_second, 1000, "max_duration")], 1100),
         step([chunk(one_second, 2000, "silence")], 2100, drain=True)])
    add("vad google: backlog of max_duration chunks is folded",
        config("Google", vad=True), [ok("folded", 0.3)],
        [step([chunk(one_second, 0, "max_duration"), chunk(one_second, 1000, "max_duration")], 1100, drain=True)])
    add("vad: padding is added to what is sent and not to what is kept",
        config("Whisper", vad=True, rate=24000, width=2), [ok("pad", 0.9, True)],
        [step([chunk(48000, 0, "silence")], 100, drain=True)])
    add("vad: padding with an odd sample rate truncates",
        config("Whisper", vad=True, rate=11025, width=3), [ok("pad", 0.9, True)],
        [step([chunk(33075, 0, "silence")], 100, drain=True)])
    add("vad: two natural segments, returns after the first",
        config("Whisper", vad=True), [ok("one", 0.9, True), ok("two", 0.9, True)],
        [step([chunk(one_second, 0, "silence"), chunk(one_second, 1000, "silence")], 1100), step([], 1200, drain=True)])
    add("vad: failed recognition raises",
        config("Whisper", vad=True), [{"error": "x"}],
        [step([chunk(one_second, 0, "silence")], 100)])
    add("vad: no real-time timeout step (queue drained): audio waits for a natural boundary",
        config("Whisper", vad=True), [],
        [step([chunk(one_second, 0, "max_duration")], 100), step([], 100000, drain=True)])
    return out


def random_scenarios(count):
    rng = random.Random(20261003)
    out = []
    for n in range(count):
        engine = rng.choice(["Google", "Whisper", "Groq_Whisper", "OpenAI_Whisper", "Custom_Whisper", "Deepgram"])
        vad = rng.random() < 0.5
        cfg = config(
            engine,
            vad=vad,
            speaker=rng.random() < 0.3,
            rate=rng.choice([16000, 16000, 44100, 48000, 8000]),
            width=rng.choice([2, 2, 2, 1, 3]),
            channels=rng.choice([1, 1, 2]),
            phrase_timeout=rng.choice([0, 1, 3, 3, 5]),
            max_phrases=rng.choice([0, 1, 2, 10]),
        )
        bps = cfg["sample_rate"] * cfg["sample_width"] * cfg["channels"]
        steps = []
        clock = 0
        used = 0
        replies = []
        for s in range(rng.randrange(2, 9)):
            chunks = []
            for _ in range(rng.choice([0, 1, 1, 2, 3, 5])):
                clock += rng.choice([0, 10, 100, 900, 1000, 2900, 3000, 3001, 5000])
                seconds = rng.choice([0, 0.01, 0.5, 1, 3, 7, 7.5, 15, 16])
                chunks.append(chunk(int(bps * seconds), clock, rng.choice(REASONS) if vad else None, seed=rng.randrange(1, 200)))
            now = clock + rng.choice([0, 100, 3000, 3001, 9000])
            languages = rng.choice([["en"], ["en", "es"], ["en", "es", "ja"]])
            countries = ["US", "ES", "JP"][: rng.choice([len(languages), len(languages), max(1, len(languages) - 1)])]
            steps.append(
                step(chunks, now, languages, countries,
                     avg_logprob=rng.choice([-0.8, -0.3]), no_speech_prob=rng.choice([0.6, 0.9]),
                     no_repeat_ngram_size=rng.choice([0, 3]), drain=rng.random() < 0.4, clear=rng.random() < 0.08)
            )
            clock = max(clock, now)
        out.append({"name": f"random {n}", "config": cfg, "replies": replies, "steps": steps})
    return out


def fill_replies(scenario, rng):
    """Give the scenario a pool of random replies, run it, and keep only the ones that were used."""
    cfg = scenario["config"]
    cloud = cfg["engine"] in ("Groq_Whisper", "OpenAI_Whisper", "Custom_Whisper", "Deepgram")
    pool = []
    for _ in range(200):
        kind = rng.random()
        if kind < 0.12:
            pool.append({"unknown": True})
        elif kind < 0.2:
            pool.append({"error": "boom"})
        elif kind < 0.3 and cloud:
            pool.append({"api_error": rng.choice(ERROR_CODES)})
        else:
            pool.append(ok(f"t{rng.randrange(1000)}", rng.choice([0.0, 0.2, 0.5, 0.5, 0.9]), rng.random() < 0.4))
    scenario["replies"] = pool
    scenario["replies"] = pool[: run_scenario(scenario, strict=False)["used"]]


def main():
    rng = random.Random(7)
    scenarios = fixed_scenarios() + random_scenarios(300)
    for scenario in scenarios:
        if scenario["name"].startswith("random"):
            fill_replies(scenario, rng)
    results = []
    bad = []
    for scenario in scenarios:
        try:
            result = run_scenario(scenario)
        except (Exception, Exhausted) as error:  # noqa: BLE001 - report every scenario that is wrongly scripted
            bad.append((scenario["name"], repr(error)))
            continue
        del result["used"]
        result["name"] = scenario["name"]
        result["replies"] = scenario["replies"]
        result["inputs"] = scenario["steps"]
        results.append(result)
    if bad:
        for name, error in bad:
            print(f"BAD SCENARIO {name}: {error}", file=sys.stderr)
        raise SystemExit(1)
    transcripts = sum(len(r["steps"][-1]["state"]["transcripts"]) for r in results)
    errors = sum(1 for r in results for s in r["steps"] if s["error"])
    sent = sum(len(s["calls"]) for r in results for s in r["steps"])
    print(f"{len(results)} scenarios, {sent} recogniser calls, {errors} raised errors, {transcripts} kept transcripts")
    (HERE / "phrases_golden.json").write_text(json.dumps({"scenarios": results}, indent=1), encoding="utf-8")


if __name__ == "__main__":
    main()
