"""Regenerate `adapter_golden.json`: what `Translator.translate` does with an engine, a language pair and the
clients it holds.

The REAL `Translator` (`models/translation/translation_translator.py`) runs with fakes where it would talk to a
service: the DeepL client, the LLM clients, the local CTranslate2 model and the `translators` web library are
replaced by objects that record what they were asked and answer from a script. Everything else is VRCT's own
code: the same-language shortcut, the language-name lookup, which client each engine uses, which calls are
made with which codes, and how a failure becomes `False` and a missing language `None`.

A scenario is the clients that exist plus a list of calls on one `Translator`; calls share state, which
matters for the LLM clients: they keep the last non-empty conversation history they were given.

A reply is `{"text": ...}`, `"false"` (the client returned False) or `"raise"` (it raised).

`tests/adapter.rs` runs the same scenarios against the Rust `NativeTranslator`.

This reads `src-python` and changes nothing in it. Run from anywhere:  python regenerate_adapter_golden.py
"""

import json
import random
import sys
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / "src-python"))

from models.translation.translation_languages import loadTranslationLanguages  # noqa: E402

table = loadTranslationLanguages(path=str(REPO / "src-python"), force=True)

import models.translation.translation_translator as module  # noqa: E402
from models.translation.translation_translator import Translator  # noqa: E402

REGISTRY = ["Plamo_API", "Gemini_API", "OpenAI_API", "Groq_API", "OpenRouter_API"]
LOCAL_SERVERS = ["OpenAI_Compatible", "LMStudio", "Ollama"]
LLM_ENGINES = REGISTRY + LOCAL_SERVERS
WEB = ["Google", "Bing", "Papago"]
EVENTS = []


class Script:
    """The answers a scenario scripts, in the order they are asked for, the last one repeating."""

    def __init__(self, replies):
        self.replies = replies
        self.asked = {}

    def next(self, key):
        replies = self.replies.get(key, ["false"])
        index = self.asked.get(key, 0)
        self.asked[key] = index + 1
        return replies[min(index, len(replies) - 1)]


class FakeLLMClient:
    def __init__(self, engine, config, script):
        self.engine, self.script = engine, script
        self.api_key, self.base_url, self.model = config["api_key"], config["base_url"], config["model"]
        self._context_history = []

    def setContextHistory(self, history):
        self._context_history = list(history)

    def translate(self, message, input_lang, output_lang):
        EVENTS.append(["llm", self.engine, self.api_key, self.base_url, self.model, message, input_lang, output_lang, len(self._context_history)])
        reply = self.script.next(self.engine)
        if reply == "raise":
            raise RuntimeError("scripted")
        return False if reply == "false" else reply["text"]


class FakeDeepL:
    def __init__(self, key, script):
        self.key, self.script = key, script

    def translate_text(self, text, source_lang=None, target_lang=None):
        EVENTS.append(["deepl", self.key, text, source_lang, target_lang])
        reply = self.script.next("DeepL_API")
        if reply in ("raise", "false"):
            raise RuntimeError("scripted")
        return SimpleNamespace(text=reply["text"])


def build(scenario):
    script = Script(scenario["replies"])
    translator = Translator()
    translator.is_enable_translators = True
    if scenario["deepl"] is not None:
        translator.deepl_client = FakeDeepL(scenario["deepl"], script)
    for engine, config in scenario["clients"].items():
        client = FakeLLMClient(engine, config, script)
        if engine in REGISTRY:
            translator._provider_clients[engine] = client
        elif engine == "OpenAI_Compatible":
            translator.openai_compatible_client = client
        elif engine == "LMStudio":
            translator.lmstudio_client = client
        else:
            translator.ollama_client = client
    translator.is_loaded_ctranslate2_model = scenario["loaded"]

    def local(message, source_language, target_language, weight_type):
        EVENTS.append(["ct2", message, source_language, target_language, weight_type])
        if translator.is_loaded_ctranslate2_model is not True:
            return False
        reply = script.next("CTranslate2")
        if reply in ("raise", "false"):
            return False
        return reply["text"]

    translator.translateCTranslate2 = local

    def web(**kwargs):
        EVENTS.append(["web", kwargs["translator"], kwargs["query_text"], kwargs["from_language"], kwargs["to_language"]])
        raise RuntimeError("the web engines are not ported")

    module.other_web_Translator = web
    module.ENABLE_TRANSLATORS = True
    return translator


def run(scenario):
    del EVENTS[:]
    translator = build(scenario)
    outcomes = []
    for call in scenario["calls"]:
        before = len(EVENTS)
        history = None if call["history"] is None else [{"source": "mic", "text": f"h{i}", "timestamp": "t"} for i in range(call["history"])]
        result = translator.translate(
            translator_name=call["engine"], weight_type=call["weight"], source_language=call["source"],
            target_language=call["target"], target_country=call["country"], message=call["message"], context_history=history,
        )
        if isinstance(result, str):
            outcome = {"text": result}
        elif result is None:
            outcome = "none"
        else:
            assert result is False, result
            outcome = "false"
        outcomes.append({"outcome": outcome, "effects": [list(e) for e in EVENTS[before:]]})
    scenario["results"] = outcomes
    return scenario


def pair(rng, engine, weight):
    """A language pair for the engine: mostly its own, sometimes one it lacks."""
    scope = table.get(engine)
    if scope is not None and engine == "CTranslate2":
        scope = scope.get(weight)
    if scope is None or rng.random() < 0.12:
        return rng.choice(["Japanese", "English", "Klingon"]), rng.choice(["English", "French", "Elvish"])
    return rng.choice(sorted(scope["source"])), rng.choice(sorted(scope["target"]))


def random_scenario(number, rng):
    clients = {}
    for engine in LLM_ENGINES:
        if rng.random() < 0.7:
            clients[engine] = {
                "api_key": rng.choice(["key-" + engine, None]) if engine in LOCAL_SERVERS else "key-" + engine,
                "base_url": rng.choice([None, "http://localhost:1234/v1"]) if engine in LOCAL_SERVERS else None,
                "model": rng.choice(["", "model-x"]),
            }
    replies = {}
    for engine in LLM_ENGINES + ["DeepL_API", "CTranslate2"]:
        options = [{"text": f"{engine} says hi"}, {"text": ""}, "false", "raise"]
        replies[engine] = [rng.choice(options) for _ in range(rng.randint(1, 3))]
    weights = sorted(table["CTranslate2"])
    calls = []
    for _ in range(rng.randint(1, 4)):
        engine = rng.choice(LLM_ENGINES + ["DeepL_API", "CTranslate2", "CTranslate2"] + WEB + ["NoSuchEngine"])
        weight = rng.choice(weights)
        source, target = pair(rng, engine, weight)
        if rng.random() < 0.1:
            target = source
        calls.append({
            "engine": engine, "weight": weight, "source": source, "target": target,
            "country": rng.choice(["United States", "Canada", "Portugal", "Brazil", "Japan", ""]),
            "message": rng.choice(["hello", "こんにちは", ""]), "history": rng.choice([None, 0, 2, 5]),
        })
    return {
        "name": f"random_{number:03d}", "deepl": rng.choice([None, "deepl-key"]), "clients": clients,
        "loaded": rng.random() < 0.7, "replies": replies, "calls": calls,
    }


def handcrafted():
    def call(engine, source="Japanese", target="English", country="United States", message="hello", history=None, weight="m2m100_418M-ct2-int8"):
        return {"engine": engine, "weight": weight, "source": source, "target": target, "country": country, "message": message, "history": history}

    client = {"api_key": "k", "base_url": None, "model": "m"}
    return [
        {"name": "same_language_is_the_message", "deepl": "k", "clients": {}, "loaded": True, "replies": {}, "calls": [call("DeepL_API", "English", "English"), call("Nonsense", "A", "A"), call("OpenAI_API", "English", "English", message="")]},
        {"name": "same_language_none_is_the_message", "deepl": None, "clients": {}, "loaded": False, "replies": {}, "calls": [call("DeepL_API", None, None)]},
        {"name": "no_client_is_a_failure", "deepl": None, "clients": {}, "loaded": True, "replies": {}, "calls": [call("DeepL_API"), call("OpenAI_API"), call("Ollama")]},
        {"name": "deepl_picks_english_variant", "deepl": "k", "clients": {}, "loaded": True, "replies": {"DeepL_API": [{"text": "x"}]}, "calls": [call("DeepL_API", "Japanese", "English", country=c) for c in ("United States", "Canada", "Philippines", "United Kingdom", "")]},
        {"name": "history_sticks_to_the_client", "deepl": None, "clients": {"OpenAI_API": client}, "loaded": True, "replies": {"OpenAI_API": [{"text": "a"}]}, "calls": [call("OpenAI_API", history=3), call("OpenAI_API", history=None), call("OpenAI_API", history=0), call("OpenAI_API", history=1)]},
        {"name": "local_model_not_loaded", "deepl": None, "clients": {}, "loaded": False, "replies": {"CTranslate2": [{"text": "x"}]}, "calls": [call("CTranslate2")]},
        {"name": "local_model_loaded", "deepl": None, "clients": {}, "loaded": True, "replies": {"CTranslate2": [{"text": "x"}]}, "calls": [call("CTranslate2"), call("CTranslate2", weight="nllb-200-distilled-600M-ct2-int8"), call("CTranslate2", "Klingon", "English")]},
        {"name": "web_engines_fail_here", "deepl": None, "clients": {}, "loaded": True, "replies": {}, "calls": [call(name) for name in WEB]},
        {"name": "unknown_engine_is_unsupported", "deepl": "k", "clients": {}, "loaded": True, "replies": {}, "calls": [call("NoSuchEngine")]},
        {"name": "unsupported_language_never_reaches_the_client", "deepl": "k", "clients": {"OpenAI_API": client}, "loaded": True, "replies": {}, "calls": [call("DeepL_API", "Klingon", "English"), call("OpenAI_API", "Japanese", "Elvish")]},
    ]


def main():
    rng = random.Random(20261005)
    scenarios = [run(s) for s in handcrafted() + [random_scenario(i, rng) for i in range(220)]]
    lines = ["{", ' "scenarios":[\n  ' + ",\n  ".join(json.dumps(s, ensure_ascii=False, separators=(",", ":")) for s in scenarios) + "\n ]", "}"]
    (HERE / "adapter_golden.json").write_text("\n".join(lines) + "\n", encoding="utf-8")
    kinds = {}
    for s in scenarios:
        for r in s["results"]:
            kinds[r["outcome"] if isinstance(r["outcome"], str) else "text"] = kinds.get(r["outcome"] if isinstance(r["outcome"], str) else "text", 0) + 1
            for e in r["effects"]:
                kinds[e[0]] = kinds.get(e[0], 0) + 1
    print(len(scenarios), "scenarios", kinds)


if __name__ == "__main__":
    main()
