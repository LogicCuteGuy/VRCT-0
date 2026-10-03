"""Regenerate `native_golden.json`: what a mic/speaker session builds from the settings.

The REAL `MicSession` / `SpeakerSession` (device choice, recorder choice, transcriber arguments, which
languages and thresholds each round of recognition is given) are lifted out of `src-python/model.py`, and the
REAL `AudioTranscriber` class (which engine it settles on) out of
`src-python/models/transcription/transcription_transcriber.py`, both by AST. Only what would open a device
or load a model is a stub, and the stubs write down the arguments they were given. A scenario is a set of
settings plus the devices that are plugged in; the result says, for the mic and for the speaker:

    device       the device the settings pick (or null)
    recorder     which recorder class it builds, and the keyword arguments
    transcriber  the engine the transcriber ends up with, and what it was built from
    ask          the languages, countries and thresholds of one round of recognition

`tests/native.rs` gives the same settings to the Rust planning functions and compares. This reads
`src-python` and changes nothing in it.  Run from anywhere:  python regenerate_native_golden.py
"""

import ast
import copy
import json
import os
import sys
import threading
from pathlib import Path
from queue import Empty, Queue
from threading import Lock, Thread, current_thread
from time import sleep
from types import SimpleNamespace
from typing import Any, Callable, Dict, List, Optional

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
SRC = REPO / "src-python"
sys.path.insert(0, str(SRC))

from errors import ERROR_METADATA, AudioPipelineError, AudioPipelineFailure, ErrorCode  # noqa: E402


def lift(path, wanted):
    source = (SRC / path).read_text(encoding="utf-8")
    pieces = {}
    for node in ast.parse(source).body:
        name = None
        if isinstance(node, ast.ClassDef):
            name = node.name
        elif isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name):
            name = node.targets[0].id
        if name in wanted:
            pieces[name] = ast.get_source_segment(source, node)
    missing = set(wanted) - set(pieces)
    assert not missing, missing
    return "\n\n".join(pieces[name] for name in wanted)


def literal(path, name):
    for node in ast.parse((SRC / path).read_text(encoding="utf-8")).body:
        if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name) and node.targets[0].id == name:
            return ast.literal_eval(node.value)
    raise KeyError(name)


CALLS = []


class Recorded:
    """A stub that writes down how it was built."""

    def __init__(self, **kwargs):
        self.kwargs = kwargs
        CALLS.append((type(self).__name__, kwargs))


def recorder_stub(name):
    return type(name, (Recorded,), {})


STATE = {"config": None, "whisper_available": False}


class ConfigProxy:
    """`config` as the lifted code sees it: attributes, read from the current scenario."""

    def __getattr__(self, name):
        return STATE["config"][name]


class DeviceManagerStub:
    def getMicDevices(self):
        return STATE["mics"]

    def getSpeakerDevices(self):
        return STATE["speakers"]


class OpenAIProviderStub:
    def __init__(self, api_key, base_url, model, engine_name):
        CALLS.append(("provider", {"api_key": api_key, "base_url": base_url, "model": model, "engine_name": engine_name}))


class DeepgramStub:
    def __init__(self, api_key, model, model_languages):
        CALLS.append(("provider", {"api_key": api_key, "model": model, "model_languages": model_languages}))


def get_whisper_model(root, weight_type, device="cpu", device_index=0, compute_type="auto"):
    CALLS.append(("whisper", {"dir": os.path.join(root, "weights", "whisper", weight_type), "device": device,
                              "device_index": device_index, "compute_type": compute_type}))
    return object()


class RecognizerStub:
    pass


API_ENGINES = tuple(literal("models/transcription/transcription_openai_compatible.py", "TRANSCRIPTION_API_ENGINES"))

NAMESPACE = {
    "Queue": Queue, "Empty": Empty, "Thread": Thread, "Lock": Lock, "current_thread": current_thread, "sleep": sleep,
    "Optional": Optional, "Callable": Callable, "Any": Any, "Dict": Dict, "List": List,
    "Event": threading.Event, "time": __import__("time"),
    "AudioPipelineError": AudioPipelineError, "AudioPipelineFailure": AudioPipelineFailure,
    "ERROR_METADATA": ERROR_METADATA, "ErrorCode": ErrorCode,
    "errorLogging": lambda: None, "printLog": lambda text: None,
    "TRANSCRIPT_STOP_JOIN_TIMEOUT": 15,
    "config": ConfigProxy(), "device_manager": DeviceManagerStub(),
    "SelectedMicVadRecorder": recorder_stub("SelectedMicVadRecorder"),
    "SelectedMicEnergyAndAudioRecorder": recorder_stub("SelectedMicEnergyAndAudioRecorder"),
    "SelectedSpeakerVadRecorder": recorder_stub("SelectedSpeakerVadRecorder"),
    "SelectedSpeakerEnergyAndAudioRecorder": recorder_stub("SelectedSpeakerEnergyAndAudioRecorder"),
    # what AudioTranscriber.__init__ touches
    "Recognizer": RecognizerStub,
    "AudioData": object,  # named in annotations only
    "getWhisperModel": get_whisper_model,
    "checkWhisperWeight": lambda root, weight_type: STATE["whisper_available"],
    "OpenAICompatibleTranscriptionProvider": OpenAIProviderStub,
    "DeepgramProvider": DeepgramStub,
    "_API_TRANSCRIPTION_ENGINES": API_ENGINES,
    "_CLOUD_TRANSCRIPTION_ENGINES": API_ENGINES + ("Deepgram",),
    "GOOGLE_RECOGNIZE_TIMEOUT_SECONDS": literal("models/transcription/transcription_transcriber.py", "GOOGLE_RECOGNIZE_TIMEOUT_SECONDS"),
}
NAMESPACE["AudioTranscriber"] = object  # only named in annotations until the real class is lifted below
exec(compile(lift("model.py", ["_AUDIO_QUEUE_MAXSIZE", "_DiscardQueue", "threadFnc", "_AudioDeviceSession", "MicSession", "SpeakerSession"]),
             "model.py(lifted)", "exec"), NAMESPACE)
exec(compile(lift("models/transcription/transcription_transcriber.py", ["AudioTranscriber"]), "transcriber(lifted)", "exec"), NAMESPACE)


# ---- scenarios --------------------------------------------------------------------------------------------

def slot(language, country, enable):
    return {"language": language, "country": country, "enable": enable}


BASE = {
    "SELECTED_MIC_HOST": "Windows WASAPI", "SELECTED_MIC_DEVICE": "Mic A", "SELECTED_SPEAKER_DEVICE": "Speaker A [Loopback]",
    "MIC_RECORD_TIMEOUT": 3, "MIC_PHRASE_TIMEOUT": 3, "MIC_THRESHOLD": 300, "MIC_AUTOMATIC_THRESHOLD": True,
    "MIC_MAX_PHRASES": 10, "MIC_ENABLE_VAD": False, "MIC_AVG_LOGPROB": -0.8, "MIC_NO_SPEECH_PROB": 0.6, "MIC_NO_REPEAT_NGRAM_SIZE": 0,
    "SPEAKER_RECORD_TIMEOUT": 3, "SPEAKER_PHRASE_TIMEOUT": 3, "SPEAKER_THRESHOLD": 300, "SPEAKER_AUTOMATIC_THRESHOLD": True,
    "SPEAKER_MAX_PHRASES": 10, "SPEAKER_ENABLE_VAD": False, "SPEAKER_AVG_LOGPROB": -0.8, "SPEAKER_NO_SPEECH_PROB": 0.6,
    "SPEAKER_NO_REPEAT_NGRAM_SIZE": 0,
    "SELECTED_TRANSCRIPTION_ENGINE": "Google", "PATH_LOCAL": "C:/vrct", "WHISPER_WEIGHT_TYPE": "base",
    "SELECTED_TRANSCRIPTION_COMPUTE_DEVICE": {"device": "cpu", "device_index": 0},
    "SELECTED_TRANSCRIPTION_COMPUTE_TYPE": "auto",
    "TRANSCRIPTION_AUTH_KEYS": {"Groq_Whisper": "gsk-1", "OpenAI_Whisper": "sk-2", "Custom_Whisper": "custom-3", "Deepgram": "dg-4"},
    "GROQ_WHISPER_BASE_URL": "https://api.groq.com/openai/v1", "OPENAI_WHISPER_BASE_URL": "https://api.openai.com/v1",
    "SELECTED_GROQ_WHISPER_MODEL": "whisper-large-v3", "SELECTED_OPENAI_WHISPER_MODEL": "whisper-1",
    "TRANSCRIPTION_CUSTOM_URL": "http://localhost:8080/v1", "SELECTED_CUSTOM_WHISPER_MODEL": "my-whisper",
    "SELECTED_DEEPGRAM_MODEL": "nova-3",
    "DEEPGRAM_MODEL_LANGUAGES": {"nova-3": ["en", "ja", "multi"], "nova-2": ["en"]},
    "SELECTED_TAB_NO": "1",
    "SELECTED_YOUR_LANGUAGES": {
        "1": {"1": slot("Japanese", "Japan", True), "2": slot("English", "United States", False), "3": slot("Korean", "South Korea", False)},
        "2": {"1": slot("French", "France", True), "2": slot("German", "Germany", True), "3": slot("Thai", "Thailand", False)},
        "3": {"1": slot("English", "United States", False), "2": slot("English", "United States", False), "3": slot("English", "United States", False)},
    },
    "SELECTED_TARGET_LANGUAGES": {
        "1": {"1": slot("English", "United States", True), "2": slot("Chinese Simplified", "China", True), "3": slot("Korean", "South Korea", False)},
        "2": {"1": slot("Spanish", "Spain", True), "2": slot("English", "United States", False), "3": slot("English", "United States", False)},
        "3": {"1": slot("English", "United States", False), "2": slot("English", "United States", False), "3": slot("English", "United States", False)},
    },
}

DEVICES = {
    "mics": {"Windows WASAPI": [{"name": "Mic A", "index": 3}, {"name": "Mic B", "index": 4}]},
    "speakers": [{"name": "Speaker A [Loopback]", "index": 7}, {"name": "Speaker B [Loopback]", "index": 8}],
}
NO_MICS = {"mics": {"NoHost": [{"name": "NoDevice"}]}, "speakers": [{"name": "NoDevice"}]}


def scenario(name, whisper=False, devices=DEVICES, **overrides):
    config = copy.deepcopy(BASE)
    config.update(copy.deepcopy(overrides))
    return {"name": name, "config": config, "devices": devices, "whisper_available": whisper}


SCENARIOS = [
    scenario("defaults"),
    scenario("vad_on_both", MIC_ENABLE_VAD=True, SPEAKER_ENABLE_VAD=True),
    scenario("vad_on_mic_only", MIC_ENABLE_VAD=True),
    scenario("vad_flag_must_be_true", MIC_ENABLE_VAD=1, SPEAKER_ENABLE_VAD="true"),
    scenario("record_timeout_longer_than_phrase_timeout", MIC_RECORD_TIMEOUT=8, MIC_PHRASE_TIMEOUT=3, SPEAKER_RECORD_TIMEOUT=9, SPEAKER_PHRASE_TIMEOUT=4),
    scenario("record_timeout_shorter", MIC_RECORD_TIMEOUT=2, MIC_PHRASE_TIMEOUT=6, SPEAKER_RECORD_TIMEOUT=1, SPEAKER_PHRASE_TIMEOUT=2),
    scenario("record_timeout_zero", MIC_RECORD_TIMEOUT=0, SPEAKER_RECORD_TIMEOUT=0),
    scenario("fixed_threshold", MIC_AUTOMATIC_THRESHOLD=False, MIC_THRESHOLD=1200, SPEAKER_AUTOMATIC_THRESHOLD=False, SPEAKER_THRESHOLD=75),
    scenario("whisper_files_present", whisper=True, SELECTED_TRANSCRIPTION_ENGINE="Whisper"),
    scenario("whisper_files_missing_falls_back_to_google", whisper=False, SELECTED_TRANSCRIPTION_ENGINE="Whisper"),
    scenario("whisper_on_gpu", whisper=True, SELECTED_TRANSCRIPTION_ENGINE="Whisper", WHISPER_WEIGHT_TYPE="large-v3",
             SELECTED_TRANSCRIPTION_COMPUTE_DEVICE={"device": "cuda", "device_index": 1}, SELECTED_TRANSCRIPTION_COMPUTE_TYPE="float16"),
    scenario("groq", SELECTED_TRANSCRIPTION_ENGINE="Groq_Whisper"),
    scenario("openai", SELECTED_TRANSCRIPTION_ENGINE="OpenAI_Whisper"),
    scenario("custom_server", SELECTED_TRANSCRIPTION_ENGINE="Custom_Whisper"),
    scenario("api_engine_without_key", SELECTED_TRANSCRIPTION_ENGINE="Groq_Whisper", TRANSCRIPTION_AUTH_KEYS={"Deepgram": "dg-4"}),
    scenario("deepgram", SELECTED_TRANSCRIPTION_ENGINE="Deepgram"),
    scenario("deepgram_model_without_languages", SELECTED_TRANSCRIPTION_ENGINE="Deepgram", SELECTED_DEEPGRAM_MODEL="nova-9"),
    scenario("deepgram_without_key", SELECTED_TRANSCRIPTION_ENGINE="Deepgram", TRANSCRIPTION_AUTH_KEYS={}),
    scenario("unknown_engine_is_google", SELECTED_TRANSCRIPTION_ENGINE="Something_Else"),
    scenario("second_tab", SELECTED_TAB_NO="2"),
    scenario("tab_without_languages", SELECTED_TAB_NO="3"),
    scenario("thresholds", MIC_AVG_LOGPROB=-1.25, MIC_NO_SPEECH_PROB=0.45, MIC_NO_REPEAT_NGRAM_SIZE=3,
             SPEAKER_AVG_LOGPROB=-0.5, SPEAKER_NO_SPEECH_PROB=0.9, SPEAKER_NO_REPEAT_NGRAM_SIZE=2),
    scenario("max_phrases_and_timeouts", MIC_MAX_PHRASES=4, MIC_PHRASE_TIMEOUT=5, SPEAKER_MAX_PHRASES=20, SPEAKER_PHRASE_TIMEOUT=7),
    scenario("other_device", SELECTED_MIC_DEVICE="Mic B", SELECTED_SPEAKER_DEVICE="Speaker B [Loopback]"),
    scenario("device_not_plugged_in", SELECTED_MIC_DEVICE="Mic Z", SELECTED_SPEAKER_DEVICE="Speaker Z [Loopback]"),
    scenario("no_device_chosen", SELECTED_MIC_DEVICE="NoDevice", SELECTED_SPEAKER_DEVICE="NoDevice"),
    scenario("no_devices_at_all", devices=NO_MICS, SELECTED_MIC_HOST="NoHost", SELECTED_MIC_DEVICE="NoDevice", SELECTED_SPEAKER_DEVICE="NoDevice"),
]


def kind_result(session_class, kind):
    session = session_class()
    session._recorder = SimpleNamespace(SAMPLE_RATE=16000, SAMPLE_WIDTH=2, channels=1)
    result = {}
    device = session._resolve_device()
    result["device"] = device["name"] if device else None

    if device is not None:
        CALLS.clear()
        session._create_recorder(device)
        (cls, kwargs), = CALLS
        kwargs = dict(kwargs)
        kwargs["device"] = kwargs["device"]["name"]
        result["recorder"] = {"class": cls, "kwargs": kwargs}

    CALLS.clear()
    transcriber = session._create_transcriber()
    provider = None
    for tag, payload in CALLS:
        provider = {"kind": tag, **payload}
    result["transcriber"] = {
        "engine": transcriber.transcription_engine,
        "provider": provider,
        "speaker": transcriber.speaker,
        "phrase_timeout": transcriber.phrase_timeout,
        "max_phrases": transcriber.max_phrases,
        "vad_segmented": transcriber.vad_segmented,
        "source": transcriber.source,
    }

    asked = {}

    class Catcher:
        def transcribeAudioQueue(self, queue, languages, countries, avg_logprob, no_speech_prob, no_repeat_ngram_size):
            asked.update(languages=languages, countries=countries, avg_logprob=avg_logprob,
                         no_speech_prob=no_speech_prob, no_repeat_ngram_size=no_repeat_ngram_size)
            return True

    session._transcribe(Catcher(), None)
    result["ask"] = asked
    return result


def run(case):
    STATE["config"] = case["config"]
    STATE["mics"] = case["devices"]["mics"]
    STATE["speakers"] = case["devices"]["speakers"]
    STATE["whisper_available"] = case["whisper_available"]
    mic = kind_result(NAMESPACE["MicSession"], "mic")
    speaker = kind_result(NAMESPACE["SpeakerSession"], "speaker")
    return {"mic": mic, "speaker": speaker}


def main():
    results = []
    for case in SCENARIOS:
        results.append({**case, "expected": run(case)})
        print(case["name"])
    out = HERE / "native_golden.json"
    out.write_text(json.dumps({"python": sys.version.split()[0], "scenarios": results}, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
