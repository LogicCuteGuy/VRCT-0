"""Regenerate `whisper_golden.json` from the real Python code.

* `features`: faster-whisper's `FeatureExtractor` (the log-mel spectrogram it feeds the model) on a
  few signals, stored as base64 little-endian float32 (`n_mels` rows of `frames` values), plus the
  mel filter bank itself.
* `provider`: `LocalWhisperProvider.transcribe` with the Whisper model replaced by a scripted one:
  what it asks the model (language and options), how it filters and joins the segments, and the
  confidence and `is_definitive` it returns.

The model run itself (encoder, decoder, tokenizer) is checked separately, against real weights, by
`regenerate_whisper_e2e_golden.py`.

Run from anywhere:  python regenerate_whisper_golden.py
"""

import base64
import hashlib
import json
import math
import sys
import types
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / "src-python"))


# ---- stand-ins ------------------------------------------------------------------------------------


class UnknownValueError(Exception):
    pass


class AudioData:
    """Holds 16 kHz mono 16-bit samples; the provider asks for them converted to exactly that."""

    def __init__(self, frame_data):
        self.frame_data = frame_data

    def get_raw_data(self, convert_rate=None, convert_width=None):
        return self.frame_data


def stub(name, **attrs):
    module = types.ModuleType(name)
    module.__dict__.update(attrs)
    sys.modules[name] = module
    return module


stub("requests", post=None, get=None, exceptions=types.SimpleNamespace(Timeout=Exception, RequestException=Exception))
stub(
    "openai",
    OpenAI=object,
    APIConnectionError=Exception,
    APIStatusError=Exception,
    APITimeoutError=Exception,
    AuthenticationError=Exception,
    RateLimitError=Exception,
)
stub("speech_recognition", AudioData=AudioData, Recognizer=object, UnknownValueError=UnknownValueError)

import models.transcription.transcription_providers as providers  # noqa: E402
from models.transcription.transcription_languages import transcription_lang  # noqa: E402
from faster_whisper.feature_extractor import FeatureExtractor  # noqa: E402


# ---- features -----------------------------------------------------------------------------------------


def b64(array) -> str:
    return base64.b64encode(np.ascontiguousarray(array, dtype="<f4").tobytes()).decode()


def signals():
    rng = np.random.default_rng(20261003)
    t = np.arange(16000 * 4) / 16000.0
    voiced = (
        0.3 * np.sin(2 * np.pi * 140 * t)
        + 0.15 * np.sin(2 * np.pi * 280 * t)
        + 0.1 * np.sin(2 * np.pi * 1200 * t * (1 + 0.1 * np.sin(2 * np.pi * 3 * t)))
    ) * (0.5 + 0.5 * np.sin(2 * np.pi * 1.7 * t)) ** 2
    return [
        ("noise 1.5 s", (rng.standard_normal(24000) * 0.05).astype(np.float32)),
        ("voiced-like 4 s", voiced.astype(np.float32)),
        ("silence 0.5 s", np.zeros(8000, dtype=np.float32)),
        ("short 0.2 s", (rng.standard_normal(3200) * 0.2).astype(np.float32)),
        ("clipped loud 1 s", np.clip(rng.standard_normal(16000) * 1.5, -1, 1).astype(np.float32)),
        ("odd length 1.234 s", (rng.standard_normal(19744) * 0.1).astype(np.float32)),
        ("one sample", np.array([0.5], dtype=np.float32)),
    ]


def feature_cases():
    cases = []
    for feature_size in (80, 128):
        extractor = FeatureExtractor(feature_size=feature_size)
        for label, wave in signals():
            if len(wave) < 41:
                continue  # numpy's reflect padding of a very short signal is not a case worth matching
            features = extractor(wave)
            cases.append(
                {
                    "label": label,
                    "feature_size": feature_size,
                    "samples": b64(wave),
                    "shape": list(features.shape),
                    "features": b64(features),
                }
            )
    filters = {
        str(size): {"shape": list(FeatureExtractor(feature_size=size).mel_filters.shape), "data": b64(FeatureExtractor(feature_size=size).mel_filters)}
        for size in (80, 128)
    }
    return cases, filters


# ---- provider -------------------------------------------------------------------------------------------


class Info:
    def __init__(self, language, language_probability):
        self.language = language
        self.language_probability = language_probability


class Segment:
    def __init__(self, text, avg_logprob, no_speech_prob):
        self.text = text
        self.avg_logprob = avg_logprob
        self.no_speech_prob = no_speech_prob


class ScriptedModel:
    def __init__(self, segments, info):
        self.segments = segments
        self.info = info
        self.calls = []

    def transcribe(self, raw, **kwargs):
        self.calls.append((raw, kwargs))
        return iter(self.segments), self.info


def outcome(call):
    try:
        text, confidence, definitive = call()
        return {"ok": {"text": text, "confidence": confidence, "definitive": definitive}}
    except Exception as error:  # noqa: BLE001 - recorded for the comparison
        return {"other": type(error).__name__}


def provider_cases():
    cases = []
    pcm = bytes((i * 37 + 11) % 256 for i in range(3200))  # 1600 samples, any bytes make valid int16
    scripts = {
        "one good segment": ([("Hello world.", -0.3, 0.05)], ("en", 0.97)),
        "two segments joined": ([(" Hello", -0.2, 0.01), (" world.", -0.5, 0.02)], ("en", 0.9)),
        "one filtered by logprob": ([(" keep", -0.2, 0.01), (" drop", -1.4, 0.01)], ("en", 0.8)),
        "one filtered by no_speech": ([(" keep", -0.2, 0.01), (" drop", -0.2, 0.95)], ("en", 0.8)),
        "all filtered": ([(" nope", -3.0, 0.99)], ("en", 0.4)),
        "no segments": ([], ("en", 0.55)),
        "edge equal": ([(" edge", -0.8, 0.6)], ("en", 0.5)),
        "other language detected": ([(" bonjour", -0.2, 0.01)], ("fr", 0.93)),
        "empty text segment": ([("", -0.2, 0.01)], ("en", 0.6)),
        "unicode": ([(" こんにちは", -0.25, 0.02)], ("ja", 0.99)),
    }
    pairs = [("English", "United States"), ("Japanese", "Japan"), ("French", "France"), ("Chinese Simplified", "China"), ("Nowhere", "Nowhere")]
    thresholds = [(-0.8, 0.6, 0), (-0.3, 0.2, 3), (-2.0, 1.0, 0)]
    for label, (segments, (language, probability)) in scripts.items():
        for lang, country in pairs:
            for force in (True, False):
                for avg_logprob, no_speech_prob, ngram in thresholds if (lang, force) == ("English", True) else thresholds[:1]:
                    model = ScriptedModel([Segment(*s) for s in segments], Info(language, probability))
                    provider = providers.LocalWhisperProvider(model)
                    result = outcome(
                        lambda: provider.transcribe(
                            AudioData(pcm),
                            lang,
                            country,
                            avg_logprob=avg_logprob,
                            no_speech_prob=no_speech_prob,
                            no_repeat_ngram_size=ngram,
                            force_language=force,
                        )
                    )
                    asked = None
                    if model.calls:
                        raw, kwargs = model.calls[0]
                        asked = {
                            "samples_sha": hashlib.sha256(np.asarray(raw, dtype="<f4").tobytes()).hexdigest(),
                            "samples": len(raw),
                            "kwargs": kwargs,
                        }
                    cases.append(
                        {
                            "script": label,
                            "segments": segments,
                            "info": {"language": language, "language_probability": probability},
                            "language": lang,
                            "country": country,
                            "force_language": force,
                            "avg_logprob": avg_logprob,
                            "no_speech_prob": no_speech_prob,
                            "no_repeat_ngram_size": ngram,
                            "asked": asked,
                            "result": result,
                        }
                    )
    return cases


def main():
    features, filters = feature_cases()
    golden = {"features": features, "mel_filters": filters, "provider": provider_cases()}
    (HERE / "whisper_golden.json").write_text(json.dumps(golden), encoding="utf-8")
    print(f"{len(features)} feature cases, {len(golden['provider'])} provider cases, numpy {np.__version__}")


if __name__ == "__main__":
    main()
