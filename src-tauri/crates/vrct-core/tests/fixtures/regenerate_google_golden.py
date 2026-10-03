"""Regenerate `google_golden.json`: what `custom_speech_recognition`'s `recognize_google` and `AudioFile` do.

The REAL code of the fork (commit afae7ff) runs here, unchanged, with only the network and the FLAC
encoder replaced:

* `replies`: `Recognizer.recognize_google(audio, language=.., with_confidence=True, join_all_results=True)`
  on many replies of the endpoint (normal ones, several blocks, missing keys, malformed ones). Recorded:
  the transcript and confidence, or "unknown" (`UnknownValueError`), or the exception's class name.
* `requests`: the request it sends (URL, headers, whether the body is the FLAC) for several sample
  rates and languages, and the `convert_rate` / `convert_width` it asks the FLAC encoder for.
* `mono`: stereo WAV files read by `AudioFile`, which mixes them down with `audioop.tomono(.., 1, 1)`
  (a clipped SUM, not a mean); the SHA-256 of the mono audio, and of what the engines get from it with
  `get_raw_data(convert_rate=16000, convert_width=2)`.
* `tomono`: `audioop.tomono(frames, width, 1, 1)` itself for every width.

The fork is not part of VRCT's repository: copy its `speech_recognition/` folder to
`src-tauri/target/custom_speech_recognition` or point `VRCT_SPEECH_RECOGNITION_FORK` at it.
Needs Python 3.12 (`audioop`, `aifc`).

Run from anywhere:  python regenerate_google_golden.py
"""

import hashlib
import io
import json
import os
import random
import struct
import sys
import wave
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
FORK = Path(os.environ.get("VRCT_SPEECH_RECOGNITION_FORK", REPO / "src-tauri" / "target" / "custom_speech_recognition"))

sys.path.insert(0, str(FORK))
import audioop  # noqa: E402
import speech_recognition as sr  # noqa: E402
from speech_recognition.audio import AudioData  # noqa: E402

assert Path(sr.__file__).resolve().is_relative_to(FORK.resolve()), f"imported {sr.__file__}, not the fork in {FORK}"
assert sr.__version__ == "3.10.4.5", sr.__version__

FLAC_STAND_IN = b"fLaC-stand-in"


class Reply:
    def __init__(self, body: bytes):
        self.body = body

    def read(self):
        return self.body


class Network:
    """Replaces `urlopen` and the FLAC encoder; remembers what was asked of them."""

    def __init__(self):
        self.body = b""
        self.requests = []
        self.flac_calls = []

    def urlopen(self, request, timeout=None):
        self.requests.append({"url": request.full_url, "headers": dict(request.header_items()), "data_is_flac": request.data == FLAC_STAND_IN, "timeout": timeout})
        return Reply(self.body)

    def get_flac_data(self, audio, convert_rate=None, convert_width=None):
        self.flac_calls.append({"convert_rate": convert_rate, "convert_width": convert_width})
        return FLAC_STAND_IN


network = Network()
sr.urlopen = network.urlopen
AudioData.get_flac_data = lambda self, convert_rate=None, convert_width=None: network.get_flac_data(self, convert_rate, convert_width)


def recognise(reply_text: str, language="ja-JP", rate=16000) -> dict:
    network.body = reply_text.encode("utf-8")
    recognizer = sr.Recognizer()
    audio = AudioData(b"\x01\x00" * 160, rate, 2)
    try:
        text, confidence = recognizer.recognize_google(audio, language=language, with_confidence=True, join_all_results=True)
    except sr.UnknownValueError:
        return {"unknown": True}
    except Exception as error:  # noqa: BLE001 - the class name is what is compared
        return {"error": type(error).__name__}
    return {"text": text, "confidence": confidence}


def block(*candidates) -> str:
    return json.dumps({"result": list(candidates)}, ensure_ascii=False)


def candidate(*alternatives) -> dict:
    return {"alternative": list(alternatives), "final": True}


def alt(transcript, confidence=None, **extra) -> dict:
    out = {"transcript": transcript}
    if confidence is not None:
        out["confidence"] = confidence
    out.update(extra)
    return out


EMPTY = block()

FIXED_REPLIES = [
    ("empty body", ""),
    ("only newlines", "\n\n\n"),
    ("empty result", EMPTY + "\n"),
    ("two empty blocks", EMPTY + "\n" + EMPTY + "\n"),
    ("one utterance", EMPTY + "\n" + block(candidate(alt("hello world", 0.9375), alt("hello word"), alt("yellow world"))) + "\n"),
    ("no confidence", block(candidate(alt("hello world"))) + "\n"),
    ("confidence zero", block(candidate(alt("hello", 0))) + "\n"),
    ("confidence one", block(candidate(alt("hello", 1))) + "\n"),
    ("confidence float", block(candidate(alt("こんにちは", 0.97321))) + "\n"),
    ("two utterances", block(candidate(alt("first part", 0.5))) + "\n" + block(candidate(alt("second part", 0.75))) + "\n"),
    ("three utterances", block(candidate(alt("a", 0.5))) + "\n" + block(candidate(alt("b", 0.25))) + "\n" + block(candidate(alt("c", 0.75))) + "\n"),
    ("empty between", block(candidate(alt("one", 0.5))) + "\n" + EMPTY + "\n" + block(candidate(alt("two", 0.25))) + "\n"),
    ("second without confidence", block(candidate(alt("one", 0.75))) + "\n" + block(candidate(alt("two"))) + "\n"),
    ("only the first candidate of a block", block(candidate(alt("kept", 0.5)), candidate(alt("dropped", 0.9))) + "\n"),
    ("no trailing newline", block(candidate(alt("x", 0.5)))),
    ("crlf", block(candidate(alt("x", 0.5))) + "\r\n" + block(candidate(alt("y", 0.25))) + "\r\n"),
    ("blank lines around", "\n" + block(candidate(alt("x", 0.5))) + "\n\n\n" + block(candidate(alt("y", 0.25))) + "\n\n"),
    ("a line of spaces", "  \n" + block(candidate(alt("x", 0.5))) + "\n"),
    ("transcript missing", block(candidate({"confidence": 0.5})) + "\n"),
    ("transcript missing then present", block(candidate({"confidence": 0.5})) + "\n" + block(candidate(alt("y", 0.25))) + "\n"),
    ("alternative empty", block({"alternative": [], "final": True}) + "\n"),
    ("alternative missing", block({"final": True}) + "\n"),
    ("candidate is a string", json.dumps({"result": ["abc"]}) + "\n"),
    ("candidate is a number", json.dumps({"result": [5]}) + "\n"),
    ("candidate is null", json.dumps({"result": [None]}) + "\n"),
    ("result is a string", json.dumps({"result": "abc"}) + "\n"),
    ("result is an empty string", json.dumps({"result": ""}) + "\n"),
    ("result is an object", json.dumps({"result": {"a": 1}}) + "\n"),
    ("result is an empty object", json.dumps({"result": {}}) + "\n"),
    ("result is a number", json.dumps({"result": 5}) + "\n"),
    ("result is null", json.dumps({"result": None}) + "\n"),
    ("no result key", json.dumps({"results": []}) + "\n"),
    ("not json", "not json\n"),
    ("bad json after good", block(candidate(alt("x", 0.5))) + "\n{oops\n"),
    ("array line", "[]\n"),
    ("object line", "{}\n"),
    ("string line", '"result"\n'),
    ("number line", "5\n"),
    ("transcript is a number", block(candidate({"transcript": 5, "confidence": 0.5})) + "\n"),
    ("transcript is null", block(candidate({"transcript": None, "confidence": 0.5})) + "\n"),
    ("confidence is null", block(candidate({"transcript": "x", "confidence": None})) + "\n"),
    ("confidence is a string", block(candidate({"transcript": "x", "confidence": "0.5"})) + "\n"),
    ("confidence is true", block(candidate({"transcript": "x", "confidence": True})) + "\n"),
    ("alternative is a string", json.dumps({"result": [{"alternative": "abc"}]}) + "\n"),
    ("alternative is an object", json.dumps({"result": [{"alternative": {"transcript": "x"}}]}) + "\n"),
    ("alternative is a number", json.dumps({"result": [{"alternative": 5}]}) + "\n"),
    ("alternative holds the word confidence", json.dumps({"result": [{"alternative": ["confidence", {"transcript": "x"}]}]}) + "\n"),
    ("best alternative is a string", json.dumps({"result": [{"alternative": ["abc"]}]}) + "\n"),
    ("best alternative is a string with the word", json.dumps({"result": [{"alternative": ["my transcript"]}]}) + "\n"),
    ("best alternative is a list", json.dumps({"result": [{"alternative": [["a"]]}]}) + "\n"),
    ("best alternative is a list with the word", json.dumps({"result": [{"alternative": [["transcript"]]}]}) + "\n"),
    ("best alternative is a number", json.dumps({"result": [{"alternative": [5]}]}) + "\n"),
    ("best alternative is null", json.dumps({"result": [{"alternative": [None]}]}) + "\n"),
    ("empty transcript", block(candidate(alt("", 0.5))) + "\n"),
    ("empty then text", block(candidate(alt("", 0.5))) + "\n" + block(candidate(alt("y", 0.25))) + "\n"),
    ("transcripts with spaces", block(candidate(alt(" lead", 0.5))) + "\n" + block(candidate(alt("trail ", 0.5))) + "\n"),
    ("unicode", block(candidate(alt("今日はいい天気ですね", 0.75))) + "\n" + block(candidate(alt("そうですね 🙂", 0.5))) + "\n"),
    ("extra keys", block(candidate(alt("x", 0.5, extra=1), alt("y"))) + "\n"),
    ("later alternative has a higher confidence", block(candidate(alt("low", 0.25), alt("high", 0.99))) + "\n"),
]


def random_replies(rng: random.Random, count: int) -> list:
    confidences = [None, 0.5, 0.25, 0.75, 0.125, 0.9375, 1, 0]
    words = ["hello", "world", "こんにちは", "good", "morning", "你好", "ok", ""]
    out = []
    for number in range(count):
        lines = []
        for _ in range(rng.randint(0, 4)):
            if rng.random() < 0.3:
                lines.append(EMPTY)
                continue
            candidates = []
            for _ in range(rng.randint(1, 2)):
                alternatives = [alt(rng.choice(words), rng.choice(confidences)) for _ in range(rng.randint(0, 3))]
                candidates.append({"alternative": alternatives, "final": True})
            lines.append(block(*candidates))
        out.append((f"random {number}", "\n".join(lines) + ("\n" if rng.random() < 0.7 else "")))
    return out


def replies() -> list:
    rng = random.Random(41)
    out = []
    for label, body in FIXED_REPLIES + random_replies(rng, 60):
        out.append({"label": label, "reply": body, "expected": recognise(body)})
    return out


def requests_made() -> list:
    table = json.loads((HERE.parents[1] / "src" / "transcription" / "assets" / "transcription_languages.json").read_text(encoding="utf-8"))
    pairs = []
    for language, countries in table.items():
        for country, engines in countries.items():
            if "Google" in engines:
                pairs.append((language, country, engines["Google"]))
    rng = random.Random(3)
    rng.shuffle(pairs)
    out = []
    for language, country, code in pairs[:12] + [("Japanese", "Japan", table["Japanese"]["Japan"]["Google"])]:
        for rate in (16000, 48000, 44100, 8000, 7999):
            network.requests.clear()
            network.flac_calls.clear()
            result = recognise(EMPTY + "\n", language=code, rate=rate)
            assert result == {"unknown": True}, result
            out.append({"language": language, "country": country, "code": code, "rate": rate, "request": network.requests[0], "flac": network.flac_calls[0]})
    return out


def wav_bytes(frames: bytes, rate: int, channels: int) -> bytes:
    stream = io.BytesIO()
    with wave.open(stream, "wb") as writer:
        writer.setnchannels(channels)
        writer.setsampwidth(2)
        writer.setframerate(rate)
        writer.writeframes(frames)
    return stream.getvalue()


def stereo_clip(rng: random.Random, frames: int, mode: str) -> bytes:
    values = []
    for _ in range(frames):
        if mode == "loud":
            left, right = rng.randint(-32768, 32767), rng.randint(-32768, 32767)
        elif mode == "extreme":
            left, right = rng.choice([-32768, 32767, 0, 16384, -16384]), rng.choice([-32768, 32767, 0, 16384, -16384])
        else:
            left, right = rng.randint(-3000, 3000), rng.randint(-3000, 3000)
        values.extend((left, right))
    return struct.pack("<" + "h" * len(values), *values)


def mono_cases() -> list:
    rng = random.Random(8)
    out = []
    for rate in (16000, 48000, 44100, 24000):
        for mode in ("quiet", "loud", "extreme"):
            for frames in (1, 200, 4097):
                frame_bytes = stereo_clip(rng, frames, mode)
                recorder = sr.Recognizer()
                with sr.AudioFile(io.BytesIO(wav_bytes(frame_bytes, rate, 2))) as source:
                    audio = recorder.record(source)
                mono = audio.frame_data
                out.append(
                    {
                        "rate": rate,
                        "mode": mode,
                        "frames": frames,
                        "stereo": frame_bytes.hex(),
                        "mono_sha": hashlib.sha256(mono).hexdigest(),
                        "mono_rate": audio.sample_rate,
                        "mono_width": audio.sample_width,
                        "pcm16k_sha": hashlib.sha256(audio.get_raw_data(convert_rate=16000, convert_width=2)).hexdigest(),
                    }
                )
    return out


def tomono_cases() -> list:
    rng = random.Random(12)
    out = []
    for width in (1, 2, 3, 4):
        limit = (1 << (8 * width - 1))
        for frames in (1, 2, 50):
            for style in ("random", "extreme"):
                samples = []
                for _ in range(frames * 2):
                    if style == "extreme":
                        samples.append(rng.choice([-limit, limit - 1, 0, limit // 2, -(limit // 2), 1, -1]))
                    else:
                        samples.append(rng.randint(-limit, limit - 1))
                data = b"".join(value.to_bytes(width, "little", signed=True) for value in samples)
                out.append({"width": width, "stereo": data.hex(), "mono": audioop.tomono(data, width, 1, 1).hex()})
    return out


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    golden = {
        "fork": "misyaguziya/custom_speech_recognition@afae7ff78cd5da36e864122f16972d2b0044e03f",
        "flac_stand_in": FLAC_STAND_IN.decode(),
        "replies": replies(),
        "requests": requests_made(),
        "mono": mono_cases(),
        "tomono": tomono_cases(),
    }
    (HERE / "google_golden.json").write_text(json.dumps(golden, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
    kinds = {}
    for case in golden["replies"]:
        key = next(iter(case["expected"]))
        kinds[key] = kinds.get(key, 0) + 1
    print(len(golden["replies"]), "replies", kinds, "|", len(golden["requests"]), "requests |", len(golden["mono"]), "mono |", len(golden["tomono"]), "tomono")


if __name__ == "__main__":
    main()
