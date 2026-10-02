"""Regenerate the language table asset and the golden language-code lookups.

Writes two files:

* `src/translation/assets/languages.json` (shipped inside the Rust binary): the
  same table Python loads from `languages.yml`, as JSON.
* `tests/fixtures/language_codes_golden.json` (tests only): what the real
  `Translator.getLanguageCode` returns for a set of lookups, or `null` where it
  raises `UnsupportedLanguageError`. One row per lookup:
  `[engine, weight_type, country, source, target, expected]`.

A lookup is two independent dictionary reads, so every source is paired with
one fixed valid target and every target with one fixed valid source; that
covers each table entry once. Unknown names on either side, and every country
case of DeepL's English/Portuguese special-casing, are added on top.

Run from anywhere:  python regenerate_languages_golden.py
"""

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
CORE = HERE.parents[1]
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / "src-python"))

from models.translation.translation_languages import loadTranslationLanguages  # noqa: E402

table = loadTranslationLanguages(path=str(REPO / "src-python"), force=True)

from models.translation.translation_translator import Translator, UnsupportedLanguageError  # noqa: E402

COUNTRIES = ["United States", "Canada", "Philippines", "United Kingdom", "Japan", "Portugal", "Brazil", ""]


def scopes():
    """(engine, weight_type, source map, target map) for every table entry."""
    for engine, entry in table.items():
        if "source" in entry:
            yield engine, "", entry["source"], entry["target"]
        else:
            for weight_type, languages in entry.items():
                yield engine, weight_type, languages["source"], languages["target"]


def lookup(engine, weight_type, country, source, target):
    try:
        return list(Translator.getLanguageCode(engine, weight_type, country, source, target))
    except UnsupportedLanguageError:
        return None


def main() -> None:
    cases = []

    def add(engine, weight_type, country, source, target):
        cases.append([engine, weight_type, country, source, target, lookup(engine, weight_type, country, source, target)])

    for engine, weight_type, sources, targets in scopes():
        first_source, first_target = next(iter(sources)), next(iter(targets))
        for source in sources:
            add(engine, weight_type, "United States", source, first_target)
        for target in targets:
            add(engine, weight_type, "United States", first_source, target)
        add(engine, weight_type, "United States", "Klingon", first_target)
        add(engine, weight_type, "United States", first_source, "Klingon")
        add(engine, weight_type, "United States", "", "")
        if engine == "DeepL_API":
            for country in COUNTRIES:
                for target in ("English", "Portuguese", "Japanese"):
                    add(engine, weight_type, country, first_source, target)
    # An engine that is not in the table at all.
    add("Nope", "", "Japan", "Japanese", "English")
    # CTranslate2 with a weight type that is not in the table.
    add("CTranslate2", "no-such-weights", "Japan", "Japanese", "English")

    assets = CORE / "src" / "translation" / "assets" / "languages.json"
    assets.write_text(json.dumps(table, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    golden = HERE / "language_codes_golden.json"
    rows = ",\n".join(json.dumps(case, ensure_ascii=False, separators=(",", ":")) for case in cases)
    golden.write_text("[\n" + rows + "\n]\n", encoding="utf-8")
    unsupported = sum(1 for case in cases if case[-1] is None)
    print(f"{len(cases)} lookups ({unsupported} unsupported) -> {golden.name}; table -> {assets.name}")


if __name__ == "__main__":
    main()
