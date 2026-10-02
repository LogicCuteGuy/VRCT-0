"""Regenerate the tiny CTranslate2 fixture and what Python's `translateCTranslate2` returns on it.

No download is involved. Everything is built locally:

* a SentencePiece model trained on a few multilingual lines,
* a randomly initialised M2M100 model small enough to commit (a few hundred KB),
  converted with CTranslate2's own converter,
* `golden.json`: for each (text, source, target) the source tokens, the target
  tokens and the final string produced by the same steps as
  `Translator.translateCTranslate2` (tokenizer -> `translate_batch` with a target
  prefix -> `hypotheses[0][1:]` -> decode); plus `decode_cases`, token lists run
  through the tokenizer's `decode` alone, to cover punctuation clean-up, unknown
  pieces and special tokens that a random model rarely produces.

The model translates nothing meaningful; it only has to be deterministic, so
Rust and Python can be compared token for token. Weights are scaled up so the
output distributions are sharp and a rounding difference between two builds of
CTranslate2 cannot flip a choice.

Output (all under `tests/fixtures/ct2_tiny/`):

    model/       CTranslate2 model (model.bin, config.json, shared_vocabulary.json)
    tokenizer/   vocab.json, sentencepiece.bpe.model, tokenizer_config.json
    golden.json

Run from anywhere:  python regenerate_ct2_golden.py
"""

import json
import random
import shutil
import sys
import tempfile
from pathlib import Path

import ctranslate2
import sentencepiece
import torch
import transformers
from transformers import M2M100Config, M2M100ForConditionalGeneration, M2M100Tokenizer
from transformers.models.m2m_100.tokenization_m2m_100 import FAIRSEQ_LANGUAGE_CODES

HERE = Path(__file__).resolve().parent
OUT = HERE / "ct2_tiny"

CORPUS = [
    "こんにちは、元気ですか？",
    "今日はいい天気ですね。",
    "ありがとうございます。また明日会いましょう。",
    "VRChatでワールドを探しています。",
    "Hello, how are you today?",
    "The weather is really nice, isn't it?",
    "Thank you very much. See you tomorrow!",
    "I'm looking for a world in VRChat.",
    "你好，今天过得怎么样？",
    "谢谢你，明天见。",
    "안녕하세요, 오늘 기분이 어떠세요?",
    "감사합니다. 내일 만나요.",
    "Bonjour, comment allez-vous aujourd'hui ?",
    "Merci beaucoup, à demain !",
    "Hallo, wie geht es dir heute?",
    "Vielen Dank, bis morgen.",
    "Hola, ¿cómo estás hoy?",
    "Muchas gracias, hasta mañana.",
]
MADEUP_WORDS = 8
# Pieces that decode to the patterns `clean_up_tokenization` rewrites (" .", " ,",
# " ' ", " n't", " 's", ...); a corpus this small would never merge them itself.
CLEAN_UP_PIECES = ["▁.", "▁,", "▁'", "▁n't", "▁'m", "▁'s", "▁'ve", "▁'re", "▁?", "▁!"]
# A random model never learns to stop, and its position table is short; cap the output.
MAX_DECODING_LENGTH = 24

# (text, source language code, target language code)
CASES = [
    ("こんにちは、元気ですか？", "ja", "en"),
    ("今日はいい天気ですね。", "ja", "en"),
    ("ありがとうございます。", "ja", "ko"),
    ("Hello, how are you today?", "en", "ja"),
    ("Thank you very much.", "en", "fr"),
    ("I'm looking for a world in VRChat.", "en", "de"),
    ("你好，今天过得怎么样？", "zh", "en"),
    ("안녕하세요, 오늘 기분이 어떠세요?", "ko", "ja"),
    ("Bonjour, comment allez-vous aujourd'hui ?", "fr", "en"),
    ("Hallo, wie geht es dir heute?", "de", "es"),
    ("Hola, ¿cómo estás hoy?", "es", "ja"),
    ("VRChatでワールドを探しています。", "ja", "zh"),
    # Characters the SentencePiece model never saw become <unk>.
    ("Ωmega ☃ 🙂", "en", "ja"),
    # Whitespace and punctuation edge cases.
    ("  spaced   out  ", "en", "ja"),
    ("Hi !", "en", "ja"),
    ("a", "en", "ja"),
]


DECODE_CASES = [
    ["▁H", "i", "__en__", "▁H", "i"],
    ["__en__"],
    ["▁H", "i", "<unk>", "▁H", "i"],
    ["▁H", "i", "</s>", "▁H", "i"],
    ["▁H", "i", "<pad>", "▁H", "i"],
    ["▁H", "i", "<s>"],
    ["__madeupword0001__", "▁H"],
    ["▁H", "i", "▁!", "▁,", "▁."],
    ["▁I", "▁'m", "▁a", "▁,", "▁we", "▁'ve", "▁it", "▁.", "▁they", "▁'re", "▁do", "▁n't", "▁it", "▁'s", "▁?", "▁!"],
    ["▁a", "▁'", "▁b", "▁'", "▁c"],
    # Edges that only the final strip() changes.
    ["▁H", "i", "▁"],
    ["▁H", "i", "▁", "▁"],
    ["▁", "▁H", "i"],
    ["▁I", "'", "m", "▁s", "ure", "▁", "?"],
    ["▁H", "i", " "],
    ["", "▁H"],
    ["▁", "▁", "▁H"],
    [],
    ["▁he", "llo", "▁?", "▁!", "▁Th", "ank", "▁you", "▁,", "▁ok", "▁."],
    ["▁d", "on", "'t", "▁d", "o", "▁i", "t", "'s"],
    ["▁Y", "ou", "'re", "▁g", "o", "od", "▁I", "'ve", "▁s", "ee", "▁I", "'m"],
    ["こ", "ん", "▁に", "ち", "は", "。"],
    ["▁こ", "んに", "ちは", "、", "元", "気です", "か", "?"],
    ["Ωmega"],
]


def train_sentencepiece(workdir: Path) -> Path:
    corpus = workdir / "corpus.txt"
    corpus.write_text("\n".join(CORPUS) + "\n", encoding="utf-8")
    prefix = workdir / "sentencepiece.bpe"
    sentencepiece.SentencePieceTrainer.train(
        input=str(corpus),
        model_prefix=str(prefix),
        vocab_size=260,
        model_type="bpe",
        character_coverage=1.0,
        hard_vocab_limit=False,
        user_defined_symbols=CLEAN_UP_PIECES,
        unk_id=3,
        bos_id=0,
        eos_id=2,
        pad_id=1,
        minloglevel=2,
    )
    return Path(f"{prefix}.model")


def write_tokenizer(spm_model: Path, directory: Path) -> M2M100Tokenizer:
    directory.mkdir(parents=True, exist_ok=True)
    shutil.copy(spm_model, directory / "sentencepiece.bpe.model")
    processor = sentencepiece.SentencePieceProcessor(model_file=str(spm_model))
    vocab = {"<s>": 0, "<pad>": 1, "</s>": 2, "<unk>": 3}
    for piece_id in range(processor.get_piece_size()):
        piece = processor.id_to_piece(piece_id)
        if piece not in vocab:
            vocab[piece] = len(vocab)
    for index in range(MADEUP_WORDS):
        vocab[f"__madeupword{index:04d}__"] = len(vocab)
    (directory / "vocab.json").write_text(json.dumps(vocab, indent=1) + "\n", encoding="utf-8")
    (directory / "tokenizer_config.json").write_text(
        json.dumps({"tokenizer_class": "M2M100Tokenizer", "clean_up_tokenization_spaces": True}, indent=1) + "\n",
        encoding="utf-8",
    )
    return M2M100Tokenizer.from_pretrained(str(directory))


# CTranslate2 4.6's converter reads `tokenizer.additional_special_tokens`, which
# transformers 5 no longer exposes. The old attribute was the language tokens in order.
M2M100Tokenizer.additional_special_tokens = property(
    lambda self: [self.get_lang_token(code) for code in FAIRSEQ_LANGUAGE_CODES["m2m100"]]
)


def build_model(tokenizer: M2M100Tokenizer, workdir: Path) -> Path:
    torch.manual_seed(7)
    config = M2M100Config(
        vocab_size=len(tokenizer) + len(FAIRSEQ_LANGUAGE_CODES["m2m100"]) + MADEUP_WORDS,
        d_model=16,
        encoder_layers=1,
        decoder_layers=1,
        encoder_attention_heads=2,
        decoder_attention_heads=2,
        encoder_ffn_dim=32,
        decoder_ffn_dim=32,
        max_position_embeddings=64,
        pad_token_id=1,
        bos_token_id=0,
        eos_token_id=2,
        decoder_start_token_id=2,
    )
    model = M2M100ForConditionalGeneration(config)
    with torch.no_grad():
        for parameter in model.parameters():
            if parameter.dim() > 1:
                parameter.normal_(0.0, 1.2)
    source = workdir / "hf_model"
    model.save_pretrained(source)
    tokenizer.save_pretrained(source)
    target = workdir / "ct2_model"
    ctranslate2.converters.TransformersConverter(str(source)).convert(str(target), quantization="float32", force=True)
    return target


def translate(translator, tokenizer, text: str, source: str, target: str) -> dict:
    """The body of `Translator.translateCTranslate2`, step for step."""
    tokenizer.src_lang = source
    source_tokens = tokenizer.convert_ids_to_tokens(tokenizer.encode(text))
    target_prefix = [tokenizer.lang_code_to_token[target]]
    results = translator.translate_batch(
        [source_tokens], target_prefix=[target_prefix], max_decoding_length=MAX_DECODING_LENGTH
    )
    hypothesis = results[0].hypotheses[0]
    target_tokens = hypothesis[1:]
    output = tokenizer.decode(tokenizer.convert_tokens_to_ids(target_tokens))
    return {
        "text": text,
        "source": source,
        "target": target,
        "source_tokens": source_tokens,
        "hypothesis": hypothesis,
        "output": output,
    }


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    with tempfile.TemporaryDirectory() as scratch:
        workdir = Path(scratch)
        spm_model = train_sentencepiece(workdir)
        tokenizer = write_tokenizer(spm_model, workdir / "tokenizer")
        model_dir = build_model(tokenizer, workdir)

        translator = ctranslate2.Translator(str(model_dir), device="cpu", compute_type="float32", inter_threads=1, intra_threads=1)
        golden = [translate(translator, tokenizer, *case) for case in CASES]
        # Random runs of real pieces, plus tokens the vocabulary lacks (they must come back as <unk>).
        rng = random.Random(5)
        pool = [token for token in tokenizer.encoder if not token.startswith("__madeup")]
        pool += ["?!", "▁ ", "x" * 3]
        random_runs = [rng.choices(pool, k=rng.randint(2, 14)) for _ in range(60)]
        decode_cases = [
            {"tokens": tokens, "output": tokenizer.decode(tokenizer.convert_tokens_to_ids(tokens))}
            for tokens in DECODE_CASES + random_runs
        ]

        if OUT.exists():
            shutil.rmtree(OUT)
        shutil.copytree(model_dir, OUT / "model")
        shutil.copytree(workdir / "tokenizer", OUT / "tokenizer", ignore=shutil.ignore_patterns("special_tokens_map.json", "added_tokens.json"))

    document = {
        "max_decoding_length": MAX_DECODING_LENGTH,
        "languages": sorted(FAIRSEQ_LANGUAGE_CODES["m2m100"]),
        "cases": golden,
        "decode_cases": decode_cases,
    }
    (OUT / "golden.json").write_text(json.dumps(document, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    size = sum(path.stat().st_size for path in OUT.rglob("*") if path.is_file())
    print(f"{len(golden)} cases, {size / 1024:.0f} KB -> {OUT} (transformers {transformers.__version__}, ctranslate2 {ctranslate2.__version__})")
    for case in golden[:4]:
        print(case["source_tokens"], "->", case["hypothesis"], "->", repr(case["output"]))


if __name__ == "__main__":
    sys.exit(main())
