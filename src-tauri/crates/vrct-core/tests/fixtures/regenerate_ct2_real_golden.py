"""Regenerate `ct2_m2m100_real_golden.json`: what Python's `translateCTranslate2` returns on the real
`jncraton/m2m100_418M-ct2-int8` model.

Nothing is downloaded. The model folder is read from `VRCT_M2M100_MODEL`, or
`~/Downloads/m2m100_418M-ct2-int8` (a `git clone` of the Hugging Face repo: `model.bin`,
`sentencepiece.bpe.model`, `vocab.json`, `tokenizer_config.json`, ...). The Rust test
(`tests/translation_ct2_real.rs`) skips unless it finds a folder whose `model.bin` has the recorded
size and SHA-256, so the golden belongs to exactly this model.

Two runs are recorded: `cases` with `compute_type="auto"` (what VRCT uses) and `float32_outputs`.

The steps are those of `Translator.translateCTranslate2` (tokenizer -> `translate_batch` with the
target-language prefix -> `hypotheses[0][1:]` -> decode), with the settings VRCT loads the model with
(`device="cpu"`, `compute_type="auto"`, `inter_threads=1`, `intra_threads=4`).

VRCT pins `transformers` 4.40.2, which cleans up tokenization spaces by default; the local `transformers`
may not, so the decode asks for it explicitly.

Run from anywhere:  python regenerate_ct2_real_golden.py
"""

import hashlib
import json
import os
import sys
from pathlib import Path

import ctranslate2
import transformers
from transformers import M2M100Tokenizer

HERE = Path(__file__).resolve().parent


def model_dir() -> Path:
    given = os.environ.get("VRCT_M2M100_MODEL")
    path = Path(given) if given else Path.home() / "Downloads" / "m2m100_418M-ct2-int8"
    if not (path / "model.bin").exists():
        sys.exit(f"no M2M100 model at {path} (set VRCT_M2M100_MODEL)")
    return path


# (text, source language code, target language code)
CASES = [
    ("こんにちは、元気ですか？", "ja", "en"),
    ("今日はいい天気ですね。", "ja", "en"),
    ("ありがとうございます。また明日会いましょう。", "ja", "en"),
    ("VRChatでワールドを探しています。", "ja", "en"),
    ("このインスタンスに入ってもいいですか？", "ja", "ko"),
    ("お願いします", "ja", "zh"),
    ("Hello, how are you today?", "en", "ja"),
    ("The weather is really nice, isn't it?", "en", "ja"),
    ("Thank you very much. See you tomorrow!", "en", "ja"),
    ("I'm looking for a world in VRChat.", "en", "ja"),
    ("Can I join your instance?", "en", "fr"),
    ("Please be quiet, the event is starting soon.", "en", "de"),
    ("Where is the nearest bathroom?", "en", "es"),
    ("What time does the show start tonight?", "en", "ko"),
    ("I don't think we've met before, I'm new here.", "en", "zh"),
    ("你好，今天过得怎么样？", "zh", "en"),
    ("谢谢你，明天见。", "zh", "ja"),
    ("안녕하세요, 오늘 기분이 어떠세요?", "ko", "en"),
    ("감사합니다. 내일 만나요.", "ko", "ja"),
    ("Bonjour, comment allez-vous aujourd'hui ?", "fr", "en"),
    ("Merci beaucoup, à demain !", "fr", "ja"),
    ("Hallo, wie geht es dir heute?", "de", "en"),
    ("Vielen Dank, bis morgen.", "de", "ja"),
    ("Hola, ¿cómo estás hoy?", "es", "en"),
    ("Muchas gracias, hasta mañana.", "es", "ja"),
    ("Привет, как дела сегодня?", "ru", "en"),
    ("สวัสดีครับ วันนี้เป็นอย่างไรบ้าง", "th", "en"),
    ("Xin chào, bạn khỏe không?", "vi", "en"),
    # Digits, symbols, emoji, characters outside the vocabulary.
    ("Meet me at 7:30 pm in world #42 (bring 3 friends).", "en", "ja"),
    ("Ωmega ☃ 🙂 test", "en", "ja"),
    ("😀😀😀", "en", "ja"),
    # Whitespace and punctuation edges.
    ("  spaced   out  ", "en", "ja"),
    ("Hi !", "en", "ja"),
    ("a", "en", "ja"),
    ("...", "en", "ja"),
    ("Is it ok , really ?", "en", "fr"),
    # Longer text, several sentences.
    (
        "Welcome everyone to the world. We are going to talk about how translation works while you speak, "
        "one sentence at a time. If you pause for a moment, the next phrase starts fresh.",
        "en",
        "ja",
    ),
    (
        "昨日は友達とVRChatで遊びました。新しいワールドがとても綺麗で、時間を忘れてしまいました。"
        "また今度、みんなで一緒に行きたいです。",
        "ja",
        "en",
    ),
]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def translate(translator, tokenizer, text: str, source: str, target: str) -> dict:
    """The body of `Translator.translateCTranslate2`, step for step."""
    tokenizer.src_lang = source
    source_tokens = tokenizer.convert_ids_to_tokens(tokenizer.encode(text))
    target_prefix = [tokenizer.lang_code_to_token[target]]
    results = translator.translate_batch([source_tokens], target_prefix=[target_prefix])
    hypothesis = results[0].hypotheses[0]
    output = tokenizer.decode(tokenizer.convert_tokens_to_ids(hypothesis[1:]), clean_up_tokenization_spaces=True)
    return {"text": text, "source": source, "target": target, "source_tokens": source_tokens, "hypothesis": hypothesis, "output": output}


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    directory = model_dir()
    tokenizer = M2M100Tokenizer.from_pretrained(str(directory), local_files_only=True)
    translator = ctranslate2.Translator(str(directory), device="cpu", compute_type="auto", inter_threads=1, intra_threads=4)
    cases = [translate(translator, tokenizer, *case) for case in CASES]
    # float32 takes no int8 kernels, so Rust and Python agree on it to the last beam choice; the int8 run is
    # where ruy (Rust) and MKL (Python) pick differently on near ties.
    exact = ctranslate2.Translator(str(directory), device="cpu", compute_type="float32", inter_threads=1, intra_threads=4)
    float32_outputs = [translate(exact, tokenizer, *case)["output"] for case in CASES]
    document = {
        "model": "jncraton/m2m100_418M-ct2-int8",
        "model_bin_size": (directory / "model.bin").stat().st_size,
        "model_bin_sha256": sha256(directory / "model.bin"),
        "tokenizer_files": {name: sha256(directory / name) for name in ("sentencepiece.bpe.model", "vocab.json", "tokenizer_config.json")},
        "compute_type": "auto",
        "transformers": transformers.__version__,
        "ctranslate2": ctranslate2.__version__,
        "cases": cases,
        "float32_outputs": float32_outputs,
    }
    (HERE / "ct2_m2m100_real_golden.json").write_text(json.dumps(document, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    print(f"{len(cases)} cases (transformers {transformers.__version__}, ctranslate2 {ctranslate2.__version__})")
    for case in cases:
        print(f"{case['source']}->{case['target']}", ascii(case["text"][:40]), "=>", ascii(case["output"][:90]))


if __name__ == "__main__":
    sys.exit(main())
