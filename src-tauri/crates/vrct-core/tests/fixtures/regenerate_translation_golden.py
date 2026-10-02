"""Regenerate the translation assets and golden prompts from the real Python clients.

Writes two files:

* `src/translation/assets/prompts.json` (shipped inside the Rust binary): per
  engine, the system prompt template, history settings and the ordered list of
  supported source languages, read from the same YAML files Python uses.
* `tests/fixtures/translation_golden.json` (tests only): the `messages` the
  real Python client builds for a set of inputs, captured by swapping the
  client's LLM for a recorder.

The Rust prompt builder must reproduce Python's system prompt byte for byte, so
the expected text comes from running Python, not from a copy of its logic.

Run from anywhere:  python regenerate_translation_golden.py
"""

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / "src-python"))

from models.translation.translation_languages import loadTranslationLanguages  # noqa: E402
from models.translation.translation_utils import loadTranslatePromptConfig  # noqa: E402

translation_lang = loadTranslationLanguages(path=str(REPO / "src-python"), force=True)

from models.translation.translation_gemini import GeminiClient  # noqa: E402
from models.translation.translation_groq import GroqClient  # noqa: E402
from models.translation.translation_lmstudio import LMStudioClient  # noqa: E402
from models.translation.translation_ollama import OllamaClient  # noqa: E402
from models.translation.translation_openai import OpenAIClient  # noqa: E402
from models.translation.translation_openai_compatible import OpenAICompatibleClient  # noqa: E402
from models.translation.translation_openrouter import OpenRouterClient  # noqa: E402
from models.translation.translation_plamo import PlamoClient  # noqa: E402

# engine (the key of `translation_lang` and what Python passes to Rust) ->
# (prompt file, client class). One entry per LLM engine.
ENGINES = {
    "OpenAI_API": ("translation_openai.yml", OpenAIClient),
    "OpenAI_Compatible": ("translation_openai_compatible.yml", OpenAICompatibleClient),
    "Groq_API": ("translation_groq.yml", GroqClient),
    "OpenRouter_API": ("translation_openrouter.yml", OpenRouterClient),
    "Plamo_API": ("translation_plamo.yml", PlamoClient),
    "LMStudio": ("translation_lmstudio.yml", LMStudioClient),
    "Ollama": ("translation_ollama.yml", OllamaClient),
    "Gemini_API": ("translation_gemini.yml", GeminiClient),
}

prompts = {}
for engine, (filename, _) in ENGINES.items():
    config = loadTranslatePromptConfig(str(REPO / "src-python"), filename)
    prompts[engine] = {
        "system_prompt": config["system_prompt"],
        "history": config["history"],
        "supported_languages": list(translation_lang[engine]["source"].keys()),
    }

assets_path = HERE.parents[1] / "src" / "translation" / "assets" / "prompts.json"
assets_path.parent.mkdir(parents=True, exist_ok=True)
assets_path.write_text(json.dumps(prompts, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")


class Recorder:
    """Stands in for the langchain chat model and keeps what it was sent."""

    def __init__(self, reply):
        self.reply = reply
        self.sent = None

    def invoke(self, messages):
        self.sent = messages

        class Reply:
            content = self.reply

        return Reply()


def history(*items):
    return [dict(zip(("source", "text", "timestamp"), item)) for item in items]


HISTORIES = {
    "none": [],
    "mixed": history(
        ("chat", "hello there", "2026-10-02T14:33:05.123456"),
        ("mic", "こんにちは", "2026-10-02T09:05:00"),
        ("speaker", "it's {braces} and 100% done", "2026-10-02T23:59:59+09:00"),
        ("ocr", "dropped, source not allowed", "2026-10-02T10:00:00"),
        ("chat", "no stamp", None),
        ("mic", "bad stamp", "yesterday"),
        ("speaker", "space separator", "2026-10-02 07:08:09"),
    ),
    "long": history(*[("chat", f"message number {i} " + "x" * 900, "2026-10-02T12:00:00") for i in range(8)]),
}
for item in HISTORIES["mixed"]:
    if item["timestamp"] is None:
        del item["timestamp"]

PAIRS = [("Japanese", "English"), ("English", "Japanese"), ("Korean", "Chinese Simplified")]
REPLIES = {
    "plain": "  translated text \n",
    "list": ["part one ", {"content": "part two"}, {"other": 1}, " end"],
}

cases = []
for engine, (filename, client_class) in ENGINES.items():
    for history_name, items in HISTORIES.items():
        for input_lang, output_lang in PAIRS[:2] if history_name != "none" else PAIRS:
            client = client_class()
            recorder = Recorder("ok")
            for attribute in ("openai_llm", "gemini_llm", "groq_llm", "openrouter_llm", "plamo_llm"):
                setattr(client, attribute, recorder)
            client.setContextHistory(items)
            text = "Hello {name} 100% [ok]\nsecond line"
            client.translate(text, input_lang, output_lang)
            cases.append(
                {
                    "engine": engine,
                    "history": items,
                    "input_lang": input_lang,
                    "output_lang": output_lang,
                    "text": text,
                    "messages": recorder.sent,
                }
            )

# What `translate` hands back for the shapes a model reply can take.
reply_cases = []
for name, reply in REPLIES.items():
    client = OpenAIClient()
    client.openai_llm = Recorder(reply)
    reply_cases.append({"name": name, "content": reply, "expected": client.translate("t", "Japanese", "English")})

# `datetime.fromisoformat(..).strftime("%H:%M")` per timestamp, "" when it raises.
# Python itself only ever writes `datetime.now().isoformat()`; the rest pins how the
# Rust parser treats other ISO 8601 spellings. (Python also takes a space before the
# zone and week dates, which Rust deliberately does not.)
from datetime import datetime  # noqa: E402

STAMPS = [
    "2026-10-02T14:33:05.123456", "2026-10-02T09:05:00", "2026-10-02T23:59:59+09:00",
    "2026-10-02 07:08:09", "2026-10-02", "20261002", "20261002T143305", "2026-10-02T14",
    "2026-10-02T14:33", "2026-10-02T1433", "2026-10-02T143305", "2026-10-02T14:33:05Z",
    "2026-10-02T14:33:05z", "2026-10-02T24:00:00", "2026-10-02T14:60:00", "2026-02-30T10:00:00",
    "2024-02-29T10:00:00", "2025-02-29T10:00:00", "2026-10-02T14:33:05.", "2026-10-02T14:33:05,5",
    "2026-10-02T14:33:05+0900", "2026-10-02T14:33:05+09", "2026-10-02T14:33:05+24:00",
    "2026-10-02T14:33:05+09:00:30", "2026-10-02T14:33:05.123456789", "yesterday", "",
    "2026-10-02X14:33:05", "0000-01-01T00:00", "2026-10-02T14:33:5", "2026-10-2T14:33:05",
    "2026-10-02T14:3:05", " 2026-10-02T14:33:05", "2026-10-02T14:33:05-00:00",
    "2026-10-02T14:33:05.1", "2026-10-02T14:33:05.12", "2026-10-02T23:59:60",
]
stamps = []
for stamp in STAMPS:
    try:
        expected = datetime.fromisoformat(stamp).strftime("%H:%M")
    except Exception:
        expected = ""
    stamps.append({"value": stamp, "expected": expected})

out = HERE / "translation_golden.json"
out.write_text(
    json.dumps({"cases": cases, "replies": reply_cases, "stamps": stamps}, ensure_ascii=False, indent=1) + "\n",
    encoding="utf-8",
)
print(f"wrote {len(prompts)} engines to {assets_path}")
print(f"wrote {len(cases)} prompt cases and {len(reply_cases)} reply cases to {out}")
