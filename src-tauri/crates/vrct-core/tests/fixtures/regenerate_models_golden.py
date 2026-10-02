"""Regenerate `models_golden.json`: which models each LLM engine offers.

For every engine this feeds one raw provider reply to the real Python
`_get_available_text_models`, with the HTTP/SDK layer swapped for a fake, and
records the list Python returns. The Rust `catalog::models` must produce the
same list from the same reply, so the expected values come from running
Python rather than from a copy of its filters.

Run from anywhere:  python regenerate_models_golden.py
"""

import json
import sys
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / "src-python"))

from models.translation import (  # noqa: E402
    translation_gemini,
    translation_groq,
    translation_lmstudio,
    translation_ollama,
    translation_openai,
    translation_openai_compatible,
    translation_openrouter,
    translation_plamo,
)

# Ids chosen to hit every filter: case, prefixes, each excluded word, fine-tunes.
OPENAI_ITEMS = [
    {"id": "gpt-4o"},
    {"id": "gpt-4o-mini"},
    {"id": "gpt-4o-audio-preview"},
    {"id": "gpt-image-1"},
    {"id": "gpt-4-vision-preview"},
    {"id": "GPT-5"},
    {"id": "o3-mini"},
    {"id": "text-embedding-3-small"},
    {"id": "whisper-1"},
    {"id": "tts-1"},
    {"id": "dall-e-3"},
    {"id": "omni-moderation-latest"},
    {"id": "gpt-4o-search-preview"},
    {"id": "gpt-4o-transcribe"},
    {"id": "gpt-4o-transcribe-diarize"},
    {"id": "ft:gpt-4o-mini:acme::abc", "root": "gpt-4o-mini-2024-07-18"},
    {"id": "ft:gpt-4o-mini:acme::img", "root": "gpt-4o-mini-2024-07-18-image"},
    {"id": "ft:babbage-002:acme::abc", "root": "babbage-002"},
    {"id": "ft:gpt-4o:acme::noroot"},
    {"id": "Llama-3.3-70B-Instruct"},
    {"id": "mistral-large-latest"},
    {"id": "claude-3-5-sonnet"},
    {"id": "RERANK-v3"},
    {"id": "Vision-Model"},
    {"id": "zeta"},
    {"id": "Zeta"},
    {"id": "日本語モデル"},
]

GEMINI_ITEMS = [
    {"name": "models/gemini-2.5-pro", "supportedGenerationMethods": ["generateContent", "countTokens"]},
    {"name": "models/gemini-2.5-flash", "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/gemini-2.5-flash-image", "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/gemini-live-2.5-flash-audio", "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/gemini-2.5-flash-preview-tts", "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/gemini-robotics-er-1.5-preview", "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/gemini-2.5-computer-use-preview", "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/veo-3.0-generate-001", "supportedGenerationMethods": ["predictLongRunning"]},
    {"name": "models/gemini-embedding-001", "supportedGenerationMethods": ["embedContent"]},
    {"name": "models/gemma-3-27b-it", "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/GEMMA-upper", "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/imagen-4.0-generate-001", "supportedGenerationMethods": ["predict"]},
    {"name": "models/text-bison-001", "supportedGenerationMethods": ["generateContent"]},
    {"name": "models/gemini-nomethods"},
    {"name": "tunedModels/models/gemini-fancy", "supportedGenerationMethods": ["generateContent"]},
]

LMSTUDIO_ITEMS = [
    {"id": "qwen2.5-7b-instruct"},
    {"id": "text-embedding-nomic-embed-text-v1.5"},
    {"id": "Llama-3.2-3B"},
    {"id": "alpha"},
]

OLLAMA_ITEMS = [
    {"name": "llama3.2:latest"},
    {"name": "gemma3:4b"},
    {"name": "nomic-embed-text:latest"},
    {"name": "Qwen3:8b"},
]


class _Fake:
    """Stands in for `OpenAI(...)`; its model list is the corpus."""

    def __init__(self, items):
        self.items = items

    def __call__(self, *args, **kwargs):
        data = [SimpleNamespace(id=item["id"], **({"root": item["root"]} if "root" in item else {})) for item in self.items]
        return SimpleNamespace(models=SimpleNamespace(list=lambda: SimpleNamespace(data=data)))


def openai_style(module, items):
    with patch.object(module, "OpenAI", _Fake(items)):
        return module._get_available_text_models("key", "http://example.invalid/v1") if module in (
            translation_openai, translation_openrouter, translation_openai_compatible
        ) else module._get_available_text_models("key")


def gemini(items):
    models = [
        SimpleNamespace(name=item["name"], supported_actions=item.get("supportedGenerationMethods", []))
        for item in items
    ]
    client = SimpleNamespace(models=SimpleNamespace(list=lambda: models))
    with patch.object(translation_gemini.genai, "Client", lambda api_key: client):
        return translation_gemini._get_available_text_models("key")


def local(module, reply):
    def get(url, **kwargs):
        return SimpleNamespace(status_code=200, json=lambda: reply)

    with patch.object(module.requests, "get", get):
        return module._get_available_text_models("http://example.invalid")


def main() -> None:
    golden = {}

    def record(engine, reply, expected):
        golden[engine] = {"reply": reply, "expected": expected}

    openai_reply = {"data": OPENAI_ITEMS}
    record("OpenAI_API", openai_reply, openai_style(translation_openai, OPENAI_ITEMS))
    record("OpenAI_Compatible", openai_reply, openai_style(translation_openai_compatible, OPENAI_ITEMS))
    record("Groq_API", openai_reply, openai_style(translation_groq, OPENAI_ITEMS))
    record("OpenRouter_API", openai_reply, openai_style(translation_openrouter, OPENAI_ITEMS))
    record("Plamo_API", openai_reply, openai_style(translation_plamo, OPENAI_ITEMS))
    record("Gemini_API", {"models": GEMINI_ITEMS}, gemini(GEMINI_ITEMS))
    record("LMStudio", {"data": LMSTUDIO_ITEMS}, local(translation_lmstudio, {"data": LMSTUDIO_ITEMS}))
    record("Ollama", {"models": OLLAMA_ITEMS}, local(translation_ollama, {"models": OLLAMA_ITEMS}))

    target = HERE / "models_golden.json"
    target.write_text(json.dumps(golden, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    for engine, entry in golden.items():
        print(f"{engine}: {len(entry['expected'])} of {len(entry['reply'].get('data', entry['reply'].get('models', [])))} kept")


if __name__ == "__main__":
    main()
