"""Regenerate `flow_golden.json`: how a message becomes translations (`Model.getTranslate` and friends).

The REAL `Model.getTranslate`, `getInputTranslate`, `getOutputTranslate` and `getTranslationHistory` are
taken out of `src-python/model.py` (by AST) and run against a scripted translator. The translator's answer
depends only on which engine, target language and country it is asked about (and on how many times it was
asked that), so the parallel calls of `getInputTranslate` give the same log whatever order the threads run
in; the log is sorted. A reply is `{"text": ...}` (a translation), `"none"` (the engine lacks the language
pair) or `"false"` (the engine failed), exactly the three things `Translator.translate` returns.

`tests/flow.rs` runs the same scenarios against the Rust port.

This reads `src-python` and changes nothing in it. Run from anywhere:  python regenerate_flow_golden.py
"""

import ast
import json
import random
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]
SRC = REPO / "src-python"

EVENTS = []
LOCK = threading.Lock()


def log_event(*event):
    with LOCK:
        EVENTS.append(list(event))


def lift_methods(names):
    source = (SRC / "model.py").read_text(encoding="utf-8")
    lines = source.splitlines()
    cls = next(n for n in ast.parse(source).body if isinstance(n, ast.ClassDef) and n.name == "Model")
    pieces = {n.name: "\n".join(lines[n.lineno - 1:n.end_lineno]) for n in cls.body if isinstance(n, ast.FunctionDef) and n.name in names}
    assert set(pieces) == set(names)
    return "class Lifted:\n" + "\n\n".join(pieces[name] for name in names)


class FakeConfig:
    def __init__(self, values):
        self.__dict__.update(values)


NAMESPACE = {
    "sleep": lambda seconds: log_event("sleep", seconds),
    "errorLogging": lambda: log_event("error_logging"),
}
exec(compile(lift_methods(["getTranslationHistory", "getTranslate", "getInputTranslate", "getOutputTranslate"]), "model.py(lifted)", "exec"), NAMESPACE)
Lifted = NAMESPACE["Lifted"]


class FakeTranslator:
    def __init__(self, scenario):
        self.scenario = scenario
        self.counters = {}

    def translate(self, translator_name, weight_type, source_language, target_language, target_country, message, context_history=None):
        key = f"{translator_name}|{target_language}|{target_country}"
        history = None if context_history is None else len(context_history)
        log_event("translate", translator_name, weight_type, source_language, target_language, target_country, message, history)
        with LOCK:
            index = self.counters.get(key, 0)
            self.counters[key] = index + 1
        replies = self.scenario["replies"].get(key, ["false"])
        reply = replies[min(index, len(replies) - 1)]
        if reply == "none":
            return None
        if reply == "false":
            return False
        return reply["text"]

    def isLoadedCTranslate2Model(self):
        return self.scenario["loaded"]


class FakeModel(Lifted):
    def __init__(self, scenario):
        self.translator = FakeTranslator(scenario)
        self.translation_history = [{"source": "mic", "text": f"h{i}", "timestamp": "t"} for i in range(scenario["history"])]
        self._translation_executor = ThreadPoolExecutor(max_workers=3)

    def ensure_initialized(self):
        pass


def language(name, country, enable=True):
    return {"language": name, "country": country, "enable": enable}


def scenario(name, call, message="hello", source=None, engine="Google", loaded=True, history=2, targets=None, yours=("Japanese", "Japan"), replies=None, weight="m2m100"):
    return {
        "name": name, "call": call, "message": message, "source": source, "engine": engine, "loaded": loaded,
        "history": history, "weight": weight,
        "yours": language(*yours),
        "targets": targets if targets is not None else [language("English", "United States")],
        "replies": replies or {},
    }


def text(value):
    return {"text": value}


def handcrafted():
    en, ko, fr = language("English", "United States"), language("Korean", "South Korea"), language("French", "France")
    off = lambda l: {**l, "enable": False}
    return [
        scenario("output_text", "output", replies={"Google|Japanese|Japan": [text("やあ")]}),
        scenario("output_source_defaults_to_the_first_target", "output", targets=[en, ko], replies={"Google|Japanese|Japan": [text("x")]}),
        scenario("output_given_source", "output", source="Korean", replies={"Google|Japanese|Japan": [text("x")]}),
        scenario("input_one_target", "input", replies={"Google|English|United States": [text("hi")]}),
        scenario("input_given_source", "input", source="French", replies={"Google|English|United States": [text("hi")]}),
        scenario("input_three_targets_run_side_by_side", "input", targets=[en, ko, fr], replies={
            "Google|English|United States": [text("a")], "Google|Korean|South Korea": [text("b")], "Google|French|France": [text("c")]}),
        scenario("input_skips_disabled_targets", "input", targets=[en, off(ko), fr], replies={
            "Google|English|United States": [text("a")], "Google|French|France": [text("c")]}),
        scenario("input_no_target_enabled", "input", targets=[off(en), off(ko)]),
        scenario("input_target_with_only_a_country", "input", targets=[language(None, "United States")], replies={"Google|None|United States": [text("a")]}),
        scenario("input_target_with_nothing_is_skipped", "input", targets=[language(None, None), en], replies={"Google|English|United States": [text("a")]}),
        scenario("unsupported_pair_keeps_the_message_after_the_local_engine_says_none_too", "output", replies={"Google|Japanese|Japan": ["none"], "CTranslate2|Japanese|Japan": ["none"]}),
        scenario("unsupported_by_the_engine_but_the_local_one_translates", "output", replies={"Google|Japanese|Japan": ["none"], "CTranslate2|Japanese|Japan": [text("local")]}),
        scenario("engine_fails_local_translates", "output", replies={"Google|Japanese|Japan": ["false"], "CTranslate2|Japanese|Japan": [text("local")]}),
        scenario("engine_fails_local_fails_once_then_works", "output", replies={"Google|Japanese|Japan": ["false"], "CTranslate2|Japanese|Japan": ["false", "false", text("late")]}),
        scenario("engine_fails_local_never_loaded_gives_up_at_once", "output", loaded=False, replies={"Google|Japanese|Japan": ["false"], "CTranslate2|Japanese|Japan": ["false"]}),
        scenario("engine_fails_local_loaded_but_failing_retries_twenty_times", "output", replies={"Google|Japanese|Japan": ["false"], "CTranslate2|Japanese|Japan": ["false"]}),
        scenario("engine_fails_local_says_none_after_a_failure", "output", replies={"Google|Japanese|Japan": ["false"], "CTranslate2|Japanese|Japan": ["false", "none"]}),
        scenario("local_engine_selected_and_failing", "output", engine="CTranslate2", replies={"CTranslate2|Japanese|Japan": ["false"]}),
        scenario("local_engine_selected_and_working", "output", engine="CTranslate2", replies={"CTranslate2|Japanese|Japan": [text("ok")]}),
        scenario("empty_translation_is_still_a_translation", "output", replies={"Google|Japanese|Japan": [text("")]}),
        scenario("no_history_is_an_empty_list_not_nothing", "output", history=0, replies={"Google|Japanese|Japan": [text("x")]}),
        scenario("long_history_is_passed_whole", "input", history=20, replies={"Google|English|United States": [text("x")]}),
        scenario("input_mixed_outcomes", "input", targets=[en, ko, fr], replies={
            "Google|English|United States": [text("a")], "Google|Korean|South Korea": ["false"], "Google|French|France": ["none"],
            "CTranslate2|Korean|South Korea": [text("ko-local")], "CTranslate2|French|France": ["none"]}),
        scenario("weight_type_goes_to_both_engines", "output", weight="nllb200_3.3b", replies={"Google|Japanese|Japan": ["false"], "CTranslate2|Japanese|Japan": [text("x")]}),
    ]


def random_scenarios(count, rng):
    pool = [("English", "United States"), ("Korean", "South Korea"), ("French", "France"), ("German", "Germany"), ("Chinese", "China")]
    result = []
    for number in range(count):
        picks = rng.sample(pool, rng.randint(0, 3))
        targets = [language(*p, rng.random() < 0.8) for p in picks]
        engine = rng.choice(["Google", "DeepL_API", "CTranslate2", "OpenAI_API"])
        replies = {}
        for target_language, country in pool + [("Japanese", "Japan")]:
            for name in (engine, "CTranslate2"):
                options = [text(f"{name}:{target_language}"), "none", "false"]
                replies[f"{name}|{target_language}|{country}"] = [rng.choice(options) for _ in range(rng.randint(1, 4))]
        result.append(scenario(
            f"random_{number:03d}", rng.choice(["input", "output"]), message=rng.choice(["hello", "こんにちは", ""]),
            source=rng.choice([None, "English", "Japanese"]), engine=engine, loaded=rng.random() < 0.7,
            history=rng.randint(0, 5), targets=targets, replies=replies,
        ))
    return result


def config_slots(targets):
    """The settings always hold three slots; the unused ones are disabled."""
    return targets + [language("English", "United States", False)] * (3 - len(targets))


def run(item):
    del EVENTS[:]
    config = FakeConfig({
        "SELECTED_TAB_NO": "1", "SELECTED_TRANSLATION_ENGINES": {"1": item["engine"]}, "CTRANSLATE2_WEIGHT_TYPE": item["weight"],
        "SELECTED_YOUR_LANGUAGES": {"1": {"1": item["yours"]}},
        "SELECTED_TARGET_LANGUAGES": {"1": {str(i + 1): t for i, t in enumerate(config_slots(item["targets"]))}},
    })
    NAMESPACE["config"] = config
    model = FakeModel(item)
    try:
        method = model.getInputTranslate if item["call"] == "input" else model.getOutputTranslate
        translation, success = method(item["message"], source_language=item["source"])
        outcome = {"translation": translation, "success": success}
    finally:
        model._translation_executor.shutdown()
    item["events"] = sorted(EVENTS, key=lambda e: json.dumps(e, ensure_ascii=False))
    item["outcome"] = outcome
    return item


def main():
    rng = random.Random(20261004)
    scenarios = [run(s) for s in handcrafted() + random_scenarios(150, rng)]
    lines = ["{", ' "scenarios":[\n  ' + ",\n  ".join(json.dumps(s, ensure_ascii=False, separators=(",", ":")) for s in scenarios) + "\n ]", "}"]
    (HERE / "flow_golden.json").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(len(scenarios), "scenarios")


if __name__ == "__main__":
    main()
