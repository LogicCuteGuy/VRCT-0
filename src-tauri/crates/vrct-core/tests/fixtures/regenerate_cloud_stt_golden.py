"""Regenerate the cloud speech-to-text assets and goldens from the real Python code.

Writes:

* `src/transcription/assets/transcription_languages.json` (shipped inside the Rust binary):
  `transcription_lang`, the table from a display language and country to each engine's code.
* `tests/fixtures/cloud_stt_golden.json` (tests only):
  - `resolve`: `resolveDeepgramLanguageCode` / `isLanguageSupportedByDeepgramModel` over every table
    entry and a set of model language lists.
  - `deepgram`: `DeepgramProvider.transcribe` with `requests.post` replaced by a scripted reply; what
    it asked (URL, headers, params, body) and what it returned or raised.
  - `openai`: `OpenAICompatibleTranscriptionProvider.transcribe` with the `openai` client replaced
    by a scripted one; what it asked (file name and type, model, language, format, temperature) and
    what it returned or raised, for the segment filtering and the confidence it makes from log-probs.
  - `models`: `getAvailableDeepgramModelsDetailed` / `getAvailableTranscriptionModels` over scripted
    model-list replies.
  - `wav`: the 44-byte header `wave` writes for a few lengths and formats.

The audio itself is stood in for by bytes the test regenerates from a seed (see `audio_bytes`);
the provider code only passes it on, so the golden records its length and SHA-256.

Run from anywhere:  python regenerate_cloud_stt_golden.py
"""

import hashlib
import io
import json
import sys
import types
import wave
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / "src-python"))

# ---- stand-ins -----------------------------------------------------------------------------------


class UnknownValueError(Exception):
    pass


class AudioData:
    """`get_wav_data` here returns the frame bytes unchanged: the provider only passes them on."""

    def __init__(self, frame_data, sample_rate=16000, sample_width=2):
        self.frame_data = frame_data

    def get_wav_data(self, convert_rate=None, convert_width=None):
        return self.frame_data


def stub(name, **attrs):
    module = types.ModuleType(name)
    module.__dict__.update(attrs)
    sys.modules[name] = module
    return module


# requests
class RequestException(Exception):
    pass


class Timeout(RequestException):
    pass


class Response:
    def __init__(self, status_code, payload):
        self.status_code = status_code
        self._payload = payload

    def json(self):
        if isinstance(self._payload, Exception):
            raise self._payload
        return self._payload

    def raise_for_status(self):
        if self.status_code != 200:
            raise RequestException(str(self.status_code))


class FakeRequests:
    """The scripted reply to the next call; every call is recorded."""

    def __init__(self):
        self.reply = None
        self.calls = []

    def post(self, url, headers=None, params=None, data=None, timeout=None):
        self.calls.append(("POST", url, headers, params, data, timeout))
        return self._answer()

    def get(self, url, headers=None, timeout=None):
        self.calls.append(("GET", url, headers, None, None, timeout))
        return self._answer()

    def _answer(self):
        reply = self.reply
        if isinstance(reply, Exception):
            raise reply
        return Response(*reply)


fake_requests = FakeRequests()
stub(
    "requests",
    post=lambda *a, **k: fake_requests.post(*a, **k),
    get=lambda *a, **k: fake_requests.get(*a, **k),
    exceptions=types.SimpleNamespace(Timeout=Timeout, RequestException=RequestException),
)


# openai
class APIError(Exception):
    pass


class APIConnectionError(APIError):
    pass


class APITimeoutError(APIConnectionError):
    pass


class APIStatusError(APIError):
    pass


class AuthenticationError(APIStatusError):
    pass


class RateLimitError(APIStatusError):
    pass


class PermissionDeniedError(APIStatusError):
    pass


class InternalServerError(APIStatusError):
    pass


class FakeOpenAI:
    reply = None
    calls = []
    models = None

    def __init__(self, api_key=None, base_url=None, timeout=None):
        self.api_key = api_key
        self.base_url = base_url
        self.audio = types.SimpleNamespace(transcriptions=types.SimpleNamespace(create=self._create))
        self.models = types.SimpleNamespace(list=self._list)

    def _create(self, **kwargs):
        FakeOpenAI.calls.append(kwargs)
        if isinstance(FakeOpenAI.reply, Exception):
            raise FakeOpenAI.reply
        return FakeOpenAI.reply

    def _list(self):
        if isinstance(FakeOpenAI.models, Exception):
            raise FakeOpenAI.models
        return types.SimpleNamespace(data=[types.SimpleNamespace(id=i) for i in FakeOpenAI.models])


stub(
    "openai",
    OpenAI=FakeOpenAI,
    APIConnectionError=APIConnectionError,
    APIStatusError=APIStatusError,
    APITimeoutError=APITimeoutError,
    AuthenticationError=AuthenticationError,
    RateLimitError=RateLimitError,
)
stub("speech_recognition", AudioData=AudioData, Recognizer=object, UnknownValueError=UnknownValueError)

import models.transcription.transcription_deepgram as dg  # noqa: E402
import models.transcription.transcription_openai_compatible as oc  # noqa: E402
import models.transcription.transcription_providers as providers  # noqa: E402
from errors import ErrorCode  # noqa: E402
from models.transcription.transcription_languages import transcription_lang  # noqa: E402


def audio_bytes(length, seed):
    period = bytes((seed + 7 * i) % 251 for i in range(251))
    return (period * (length // 251 + 1))[:length]


# ---- the language table ------------------------------------------------------------------------

ASSET = HERE.parents[1] / "src" / "transcription" / "assets" / "transcription_languages.json"


def write_table():
    ASSET.parent.mkdir(parents=True, exist_ok=True)
    ASSET.write_text(json.dumps(transcription_lang, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")


# ---- Deepgram language resolution --------------------------------------------------------------


def resolve_cases():
    rows = []
    for language, countries in transcription_lang.items():
        for country, entry in countries.items():
            google = entry.get("Google", "")
            whisper = entry.get("Whisper", "")
            lists = [
                [],
                ["multi"],
                ["multi", google],
                [google],
                [google.upper()],
                [google.lower(), "xx"],
                [whisper],
                [whisper.upper()],
                [whisper + "-XX"],
                [whisper + "-XX", "en-US"],
                ["en", "ja", "zh"],
                ["", None, whisper],
                ["en-US", "en-GB", "en-AU", "ja"],
            ]
            for model_languages in lists:
                rows.append(
                    {
                        "language": language,
                        "country": country,
                        "model_languages": model_languages,
                        "resolved": dg.resolveDeepgramLanguageCode(language, country, model_languages),
                        "supported": dg.isLanguageSupportedByDeepgramModel(language, country, model_languages),
                    }
                )
    # Entries that are not in the table.
    for language, country in [("Nowhere", "Nowhere"), ("English", "Nowhere"), ("Nowhere", "United States")]:
        for model_languages in ([], ["en"], ["multi"]):
            rows.append(
                {
                    "language": language,
                    "country": country,
                    "model_languages": model_languages,
                    "resolved": dg.resolveDeepgramLanguageCode(language, country, model_languages),
                    "supported": dg.isLanguageSupportedByDeepgramModel(language, country, model_languages),
                }
            )
    return rows


# ---- providers ---------------------------------------------------------------------------------


def outcome(call):
    try:
        text, confidence, definitive = call()
        return {"ok": {"text": text, "confidence": confidence, "definitive": definitive}}
    except providers.TranscriptionApiError as error:
        return {"api_error": error.error_code.value}
    except Exception as error:  # noqa: BLE001 - recorded for the comparison
        return {"other": type(error).__name__}


KWARGS = dict(avg_logprob=-0.8, no_speech_prob=0.6, no_repeat_ngram_size=0)


def deepgram_cases():
    cases = []
    english = ("English", "United States")
    japanese = ("Japanese", "Japan")
    ok_body = lambda transcript, confidence=0.93: {  # noqa: E731
        "results": {"channels": [{"alternatives": [{"transcript": transcript, "confidence": confidence}]}]}
    }
    scripts = [
        ("200 text", (200, ok_body("hello there", 0.91))),
        ("200 empty transcript", (200, ok_body("", 0.5))),
        ("200 transcript null", (200, ok_body(None, 0.5))),
        ("200 confidence null", (200, {"results": {"channels": [{"alternatives": [{"transcript": "x", "confidence": None}]}]}})),
        ("200 no confidence key", (200, {"results": {"channels": [{"alternatives": [{"transcript": "x"}]}]}})),
        ("200 confidence string", (200, {"results": {"channels": [{"alternatives": [{"transcript": "x", "confidence": "0.25"}]}]}})),
        ("200 no alternatives", (200, {"results": {"channels": [{"alternatives": []}]}})),
        ("200 no channels", (200, {"results": {"channels": []}})),
        ("200 no results", (200, {})),
        ("401", (401, {})),
        ("403", (403, {})),
        ("429", (429, {})),
        ("400", (400, {})),
        ("500", (500, {})),
        ("503", (503, {})),
        ("timeout", Timeout("slow")),
        ("connection", RequestException("down")),
    ]
    variants = [
        # (language, country, force_language, model, model_languages)
        (*english, True, "nova-3", ["en", "ja"]),
        (*english, True, "nova-3", ["en-US", "en-GB"]),
        (*english, True, "nova-3", ["multi"]),
        (*english, True, "nova-3", []),
        (*english, False, "nova-3", ["en"]),
        (*japanese, True, "nova-2", ["ja"]),
        ("Chinese Simplified", "China", True, "nova-3", ["zh-CN", "zh"]),
    ]
    for label, reply in scripts:
        for language, country, force, model, model_languages in variants if label == "200 text" else variants[:2]:
            fake_requests.reply = reply
            fake_requests.calls.clear()
            provider = providers.DeepgramProvider("KEY-1", model, model_languages)
            data = audio_bytes(3200, 3)
            result = outcome(
                lambda: provider.transcribe(
                    AudioData(data), language, country, force_language=force, **KWARGS
                )
            )
            method, url, headers, params, body, timeout = fake_requests.calls[0]
            cases.append(
                {
                    "script": label,
                    "reply": None if isinstance(reply, Exception) else {"status": reply[0], "body": reply[1]},
                    "raises": None if not isinstance(reply, Exception) else ("timeout" if isinstance(reply, Timeout) else "connection"),
                    "language": language,
                    "country": country,
                    "force_language": force,
                    "model": model,
                    "model_languages": model_languages,
                    "request": {
                        "url": url,
                        "headers": headers,
                        "params": params,
                        "body_len": len(body),
                        "body_sha": hashlib.sha256(body).hexdigest(),
                        "timeout": list(timeout),
                    },
                    "result": result,
                }
            )
    return cases


def segment(text, avg_logprob, no_speech_prob):
    return {"text": text, "avg_logprob": avg_logprob, "no_speech_prob": no_speech_prob}


def openai_cases():
    cases = []
    engines = [
        ("Groq_Whisper", "https://api.groq.com/openai/v1", "whisper-large-v3"),
        ("OpenAI_Whisper", "https://api.openai.com/v1", "whisper-1"),
        ("Custom_Whisper", "http://localhost:8000/v1", "my-model"),
    ]
    responses = [
        ("segments all accepted", {"segments": [segment("Hello ", -0.2, 0.01), segment("world", -0.4, 0.02)], "text": "Hello world", "language": "english"}),
        ("segments filtered by logprob", {"segments": [segment("keep ", -0.1, 0.0), segment("drop", -1.5, 0.0)], "text": "keep drop", "language": "english"}),
        ("segments filtered by no_speech", {"segments": [segment("keep ", -0.1, 0.0), segment("drop", -0.1, 0.9)], "text": "keep drop", "language": "english"}),
        ("all segments dropped", {"segments": [segment("x", -2.0, 0.9)], "text": "x", "language": "english"}),
        ("edge: logprob equal to the limit", {"segments": [segment("edge", -0.8, 0.6)], "text": "edge", "language": "english"}),
        ("segments empty falls back to text", {"segments": [], "text": "plain text", "language": "english"}),
        ("segments none falls back to text", {"segments": None, "text": "plain text", "language": None}),
        ("no segments attribute", {"text": "only text"}),
        ("empty text", {"segments": [], "text": "", "language": "english"}),
        ("text none", {"segments": [], "text": None}),
        ("language matches the table code", {"segments": [segment("hi", -0.3, 0.1)], "text": "hi", "language": "en"}),
        ("language differs", {"segments": [segment("hi", -0.3, 0.1)], "text": "hi", "language": "ja"}),
        ("very low logprob accepted", {"segments": [segment("hi", -0.79, 0.1)], "text": "hi", "language": "en"}),
        ("positive logprob", {"segments": [segment("hi", 0.5, 0.1)], "text": "hi", "language": "en"}),
        ("empty segment text", {"segments": [segment("", -0.3, 0.1)], "text": "", "language": "en"}),
    ]
    thresholds = [(-0.8, 0.6), (-0.3, 0.2), (-2.0, 1.0)]
    for engine, base_url, model in engines:
        for label, body in responses:
            for force in (True, False):
                for avg_logprob, no_speech_prob in thresholds[:3] if engine == "OpenAI_Whisper" else thresholds[:1]:
                    FakeOpenAI.calls.clear()
                    FakeOpenAI.reply = types.SimpleNamespace(**body)
                    provider = providers.OpenAICompatibleTranscriptionProvider("KEY-2", base_url, model, engine)
                    if body.get("segments"):
                        FakeOpenAI.reply.segments = [types.SimpleNamespace(**s) for s in body["segments"]]
                    data = audio_bytes(6400, 5)
                    language, country = ("English", "United States")
                    result = outcome(
                        lambda: provider.transcribe(
                            AudioData(data),
                            language,
                            country,
                            avg_logprob=avg_logprob,
                            no_speech_prob=no_speech_prob,
                            no_repeat_ngram_size=0,
                            force_language=force,
                        )
                    )
                    call = FakeOpenAI.calls[0]
                    name, content, mime = call["file"]
                    cases.append(
                        {
                            "engine": engine,
                            "base_url": base_url,
                            "model": model,
                            "script": label,
                            "reply": body,
                            "language": language,
                            "country": country,
                            "force_language": force,
                            "avg_logprob": avg_logprob,
                            "no_speech_prob": no_speech_prob,
                            "request": {
                                "file_name": name,
                                "file_type": mime,
                                "file_len": len(content),
                                "file_sha": hashlib.sha256(content).hexdigest(),
                                "model": call["model"],
                                "language": call["language"],
                                "response_format": call["response_format"],
                                "temperature": call["temperature"],
                            },
                            "result": result,
                        }
                    )
    # Failures.
    errors = [
        ("authentication", AuthenticationError("401")),
        ("rate limit", RateLimitError("429")),
        ("timeout", APITimeoutError("t")),
        ("connection", APIConnectionError("c")),
        ("permission denied", PermissionDeniedError("403")),
        ("server", InternalServerError("500")),
        ("status", APIStatusError("400")),
        ("something else", ValueError("x")),
    ]
    for label, error in errors:
        FakeOpenAI.calls.clear()
        FakeOpenAI.reply = error
        provider = providers.OpenAICompatibleTranscriptionProvider("KEY-2", "https://api.openai.com/v1", "whisper-1", "OpenAI_Whisper")
        result = outcome(
            lambda: provider.transcribe(
                AudioData(audio_bytes(320, 1)), "English", "United States", force_language=True, **KWARGS
            )
        )
        cases.append({"engine": "OpenAI_Whisper", "script": "raises " + label, "result": result})
    return cases


def model_cases():
    rows = {"deepgram": [], "openai": [], "keys": []}
    entries = [
        {"name": "nova-3", "batch": True, "languages": ["en", "ja"]},
        {"name": "nova-2", "batch": True, "languages": ["en"]},
        {"name": "flux", "batch": False, "languages": ["en"]},
        {"name": "nova-3", "batch": True, "languages": ["fr"]},
        {"name": None, "batch": True, "languages": ["en"]},
        {"name": "aura", "batch": True, "languages": None},
        {"name": "enhanced", "batch": "true", "languages": ["en"]},
    ]
    for status, body in [(200, {"stt": entries}), (200, {"stt": []}), (200, {}), (401, {}), (500, {"stt": entries})]:
        fake_requests.reply = (status, body)
        rows["deepgram"].append(
            {"status": status, "body": body, "detailed": dg.getAvailableDeepgramModelsDetailed("K"), "names": dg.getAvailableDeepgramModels("K"), "check": dg.checkDeepgramApiKey("K")}
        )
    fake_requests.reply = Timeout("t")
    rows["deepgram"].append({"status": None, "body": None, "detailed": dg.getAvailableDeepgramModelsDetailed("K"), "names": dg.getAvailableDeepgramModels("K"), "check": dg.checkDeepgramApiKey("K")})
    for ids, keywords in [
        (["whisper-large-v3", "gpt-4o", "gpt-4o-transcribe", "Whisper-1", "text-embedding-3"], ["whisper", "transcribe"]),
        (["b", "a", "c"], None),
        (["b", "a", "c"], []),
        ([], ["whisper"]),
        (["distil-whisper-large-v3-en", "WHISPER-X"], ["whisper"]),
    ]:
        FakeOpenAI.models = ids
        rows["openai"].append({"ids": ids, "keywords": keywords, "models": oc.getAvailableTranscriptionModels("K", "http://x", keywords), "check": oc.checkTranscriptionApiKey("K", "http://x")})
    FakeOpenAI.models = RuntimeError("down")
    rows["openai"].append({"ids": None, "keywords": ["whisper"], "models": "raises", "check": oc.checkTranscriptionApiKey("K", "http://x")})
    return rows


def wav_cases():
    rows = []
    for frames, rate, width, channels in [(0, 16000, 2, 1), (1, 16000, 2, 1), (160, 16000, 2, 1), (333, 16000, 2, 1), (5, 8000, 2, 1), (4, 44100, 2, 2), (7, 16000, 1, 1)]:
        buffer = io.BytesIO()
        with wave.open(buffer, "wb") as writer:
            writer.setframerate(rate)
            writer.setsampwidth(width)
            writer.setnchannels(channels)
            writer.writeframes(audio_bytes(frames * width * channels, 9))
        data = buffer.getvalue()
        rows.append({"frames": frames, "rate": rate, "width": width, "channels": channels, "header": data[:44].hex(), "total": len(data)})
    return rows


def main():
    write_table()
    golden = {
        "resolve": resolve_cases(),
        "deepgram": deepgram_cases(),
        "openai": openai_cases(),
        "models": model_cases(),
        "wav": wav_cases(),
    }
    (HERE / "cloud_stt_golden.json").write_text(json.dumps(golden, indent=1), encoding="utf-8")
    print(
        f"{len(golden['resolve'])} resolve rows, {len(golden['deepgram'])} deepgram, "
        f"{len(golden['openai'])} openai cases, table {sum(len(c) for c in transcription_lang.values())} entries"
    )


if __name__ == "__main__":
    main()
