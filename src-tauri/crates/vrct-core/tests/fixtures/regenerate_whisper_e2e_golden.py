"""Regenerate `whisper_e2e_golden.json`: faster-whisper on real weights and real (synthesised) speech.

The Rust model runner (`transcription::whisper::model`, feature `ct2`) is checked against this on a
machine that has a Whisper model: the same clips go through both and the token ids, the text, the
average log-probability, the no-speech probability and the detected language are compared.

Needs, on this machine only (nothing is downloaded):

* Windows with the stock `System.Speech` voices (David and Zira); they speak the test sentences.
  The audio is written to `src-tauri/target/whisper_fixtures/`, which is not in git: the golden stores
  its SHA-256 so the Rust test can tell it is looking at the same clip, and skips when it is not there.
* a faster-whisper model folder: `VRCT_WHISPER_MODEL_SMALL`, or `Systran/faster-whisper-small` in the
  Hugging Face cache.

Run from anywhere:  python regenerate_whisper_e2e_golden.py
"""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
FIXTURES = REPO / "src-tauri" / "target" / "whisper_fixtures"


def model_dir() -> Path:
    given = os.environ.get("VRCT_WHISPER_MODEL_SMALL")
    if given:
        return Path(given)
    snapshots = Path.home() / ".cache/huggingface/hub/models--Systran--faster-whisper-small/snapshots"
    for snapshot in sorted(snapshots.glob("*")):
        if (snapshot / "model.bin").exists():
            return snapshot
    sys.exit("no faster-whisper-small model found (set VRCT_WHISPER_MODEL_SMALL)")


SENTENCES = {
    "greeting": ("Microsoft David Desktop", "Hello, this is a test of the live translation chat box for virtual reality."),
    "fox": ("Microsoft Zira Desktop", "The quick brown fox jumps over the lazy dog. She sells sea shells by the sea shore."),
    "short": ("Microsoft Zira Desktop", "Yes."),
    "numbers": ("Microsoft David Desktop", "Please meet me at the world one at seven thirty tonight, and bring three friends."),
    "long": (
        "Microsoft David Desktop",
        " ".join(
            [
                "Welcome everyone to the world, and thank you for joining us tonight.",
                "We are going to talk about how translation works while you speak, one sentence at a time.",
                "First the microphone picks up your voice, then the speech is turned into text.",
                "After that the text is translated and shown in the chat box above your head.",
                "If you pause for a moment, the next phrase starts fresh, so please take your time.",
                "Some people speak quickly and some slowly, and both are perfectly fine for this tool.",
                "Let us try a longer passage so that the recognizer has to work through several windows.",
                "The weather in the instance is always sunny, and the music never stops playing here.",
            ]
        ),
    ),
}


def speak(voice: str, text: str, path: Path) -> None:
    script = (
        "Add-Type -AssemblyName System.Speech;"
        "$s = New-Object System.Speech.Synthesis.SpeechSynthesizer;"
        f"$s.SelectVoice('{voice}');"
        "$f = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, 'Sixteen', 'Mono');"
        f"$s.SetOutputToWaveFile('{path}', $f);"
        "$s.Speak([Console]::In.ReadToEnd());"
        "$s.Dispose();"
    )
    subprocess.run(["powershell", "-NoProfile", "-Command", script], input=text.encode("utf-8"), check=True)


def read_wav(path: Path) -> np.ndarray:
    with wave.open(str(path), "rb") as reader:
        assert reader.getframerate() == 16000 and reader.getsampwidth() == 2 and reader.getnchannels() == 1
        return np.frombuffer(reader.readframes(reader.getnframes()), dtype="<i2")


def write_wav(path: Path, pcm: np.ndarray) -> None:
    with wave.open(str(path), "wb") as writer:
        writer.setnchannels(1)
        writer.setsampwidth(2)
        writer.setframerate(16000)
        writer.writeframes(pcm.astype("<i2").tobytes())


def clips() -> dict:
    FIXTURES.mkdir(parents=True, exist_ok=True)
    out = {}
    for name, (voice, text) in SENTENCES.items():
        path = FIXTURES / f"{name}.wav"
        speak(voice, text, path)
        out[name] = read_wav(path)
    rng = np.random.default_rng(7)
    out["noise"] = (rng.standard_normal(16000 * 3) * 300).astype("<i2")
    out["silence"] = np.zeros(16000 * 2, dtype="<i2")
    # A phrase with a lead-in and tail of silence, as the segmenter hands them over.
    padded = np.concatenate([np.zeros(4800, dtype="<i2"), out["greeting"], np.zeros(8000, dtype="<i2")])
    out["greeting padded"] = padded
    for name in ("noise", "silence", "greeting padded"):
        write_wav(FIXTURES / f"{name.replace(' ', '_')}.wav", out[name])
    return out


def prompts(model) -> list:
    """`WhisperModel.get_prompt` for the languages and previous-token lists the decoding loop produces."""
    from faster_whisper.tokenizer import Tokenizer

    out = []
    for language in ("en", "ja"):
        tokenizer = Tokenizer(model.hf_tokenizer, model.model.is_multilingual, task="transcribe", language=language)
        for previous in ([], [300, 301, 302], list(range(1000, 1300))):
            out.append({"language": language, "previous": previous, "prompt": model.get_prompt(tokenizer, previous, without_timestamps=True)})
    return out


def main():
    from faster_whisper import WhisperModel
    from faster_whisper.tokenizer import Tokenizer
    import faster_whisper.transcribe as transcribe_module

    directory = model_dir()
    model = WhisperModel(str(directory), device="cpu", device_index=0, compute_type="int8", cpu_threads=4, num_workers=1, local_files_only=True)
    if sys.argv[1:] == ["--prompts-only"]:
        # Adds the prompts to an existing golden without synthesising the clips again.
        path = HERE / "whisper_e2e_golden.json"
        golden = json.loads(path.read_text(encoding="utf-8"))
        golden["prompts"] = prompts(model)
        path.write_text(json.dumps(golden, indent=1), encoding="utf-8")
        print(len(golden["prompts"]), "prompts")
        return
    tokenizer = Tokenizer(model.hf_tokenizer, model.model.is_multilingual, task="transcribe", language="en")
    suppressed = list(transcribe_module.get_suppressed_tokens(tokenizer, [-1]))

    cases = []
    for name, pcm in clips().items():
        raw = pcm.astype(np.float32) / 32768.0
        for language in (None, "en"):
            for avg_logprob, no_speech_prob, ngram in ((-0.8, 0.6, 0), (-1.0, 0.6, 3)):
                if language == "en" and ngram:
                    continue
                segments, info = model.transcribe(
                    raw,
                    beam_size=5,
                    temperature=0.0,
                    log_prob_threshold=avg_logprob,
                    no_speech_threshold=no_speech_prob,
                    language=language,
                    word_timestamps=False,
                    without_timestamps=True,
                    task="transcribe",
                    no_repeat_ngram_size=ngram,
                )
                segments = list(segments)
                cases.append(
                    {
                        "clip": name.replace(" ", "_"),
                        "clip_sha": hashlib.sha256(pcm.tobytes()).hexdigest(),
                        "samples": int(len(pcm)),
                        "language": language,
                        "avg_logprob": avg_logprob,
                        "no_speech_prob": no_speech_prob,
                        "no_repeat_ngram_size": ngram,
                        "info": {"language": info.language, "language_probability": info.language_probability},
                        "segments": [
                            {
                                "text": s.text,
                                "tokens": list(s.tokens),
                                "avg_logprob": s.avg_logprob,
                                "no_speech_prob": s.no_speech_prob,
                                "start": s.start,
                                "end": s.end,
                            }
                            for s in segments
                        ],
                    }
                )
    golden = {
        "model": "Systran/faster-whisper-small",
        "model_files": {name: hashlib.sha256((directory / name).read_bytes()).hexdigest() for name in ("config.json", "tokenizer.json")},
        "model_bin_size": (directory / "model.bin").stat().st_size,
        "compute_type": "int8",
        "suppressed_tokens": suppressed,
        "non_speech_tokens": list(tokenizer.non_speech_tokens),
        "prompts": prompts(model),
        "cases": cases,
    }
    (HERE / "whisper_e2e_golden.json").write_text(json.dumps(golden, indent=1), encoding="utf-8")
    print(f"{len(cases)} cases, {len(suppressed)} suppressed tokens")
    for case in cases:
        text = "|".join(s["text"] for s in case["segments"])
        print(case["clip"], case["language"], case["info"]["language"], f"{case['info']['language_probability']:.3f}", ascii(text[:90]))


if __name__ == "__main__":
    main()
