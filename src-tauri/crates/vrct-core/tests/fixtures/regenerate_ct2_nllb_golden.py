"""Regenerate the tiny NLLB-style CTranslate2 fixture (`ct2_tiny_nllb/`).

Same idea as `regenerate_ct2_golden.py`: nothing is downloaded. A random
M2M100-architecture model (the architecture NLLB uses) is converted with
CTranslate2, next to a hand-built `tokenizer.json` with the same parts as the
Hugging Face NLLB one: Unigram model, Metaspace pre-tokenizer and decoder,
language codes as special added tokens. What is recorded is what Python does in
`Translator.translateCTranslate2` for NLLB models:

    source = [src_code] + pieces + [</s>]        (fast tokenizer, legacy_behaviour=False)
    prefix = [target_code]
    output = decode(hypothesis[1:]) + clean_up_tokenization

The tokenizer part is run through the `tokenizers` library itself (the code the
fast tokenizer wraps, and the same version Rust uses); transformers 5 no longer
reads such a `tokenizer.json` the way 4.40 does, so it cannot be the oracle here.
The glue (prefix, suffix, unknown handling, clean-up) follows transformers 4.40.

Output under `tests/fixtures/ct2_tiny_nllb/`: model/, tokenizer/ (tokenizer.json,
tokenizer_config.json), golden.json. Run from anywhere:  python regenerate_ct2_nllb_golden.py
"""

import json
import shutil
import sys
import tempfile
from pathlib import Path

import ctranslate2
import sentencepiece
import torch
from tokenizers import AddedToken, Tokenizer, decoders, models, normalizers, pre_tokenizers, processors
from transformers import M2M100Config, M2M100ForConditionalGeneration

import regenerate_ct2_golden as m2m

HERE = Path(__file__).resolve().parent
OUT = HERE / "ct2_tiny_nllb"
LANGS = ["eng_Latn", "jpn_Jpan", "fra_Latn", "deu_Latn", "zho_Hans", "kor_Hang"]
MAX_DECODING_LENGTH = 24

CASES = [
    ("こんにちは、元気ですか？", "jpn_Jpan", "eng_Latn"),
    ("今日はいい天気ですね。", "jpn_Jpan", "fra_Latn"),
    ("Hello, how are you today?", "eng_Latn", "jpn_Jpan"),
    ("Thank you very much.", "eng_Latn", "deu_Latn"),
    ("I'm looking for a world in VRChat.", "eng_Latn", "kor_Hang"),
    ("你好，今天过得怎么样？", "zho_Hans", "eng_Latn"),
    ("Bonjour, comment allez-vous aujourd'hui ?", "fra_Latn", "eng_Latn"),
    ("Hallo, wie geht es dir heute?", "deu_Latn", "jpn_Jpan"),
    ("Ωmega ☃ 🙂", "eng_Latn", "jpn_Jpan"),
    ("  spaced   out  ", "eng_Latn", "jpn_Jpan"),
    ("Hi !", "eng_Latn", "jpn_Jpan"),
    ("a", "eng_Latn", "jpn_Jpan"),
    ("", "eng_Latn", "jpn_Jpan"),
    # A language code the tokenizer was never given is passed through untouched.
    ("Hello", "eng_Latn", "xxx_Xxxx"),
]

CLEAN_UP_PIECES = ["▁.", "▁,", "▁'", "▁n't", "▁'m", "▁'s", "▁'ve", "▁'re", "▁?", "▁!"]
DECODE_CASES = [
    ["▁Hello", ",", "▁how", "▁are", "▁you", "▁?", "</s>", "eng_Latn"],
    ["▁H", "i", "▁!", "▁,", "▁."],
    ["▁I", "▁'m", "▁a", "▁,", "▁we", "▁'ve", "▁it", "▁.", "▁they", "▁'re", "▁do", "▁n't", "▁it", "▁'s", "▁?", "▁!"],
    ["▁a", "▁'", "▁b", "▁'", "▁c"],
    ["▁H", "i", "▁"],
    ["▁", "▁H", "i"],
    ["eng_Latn", "▁H", "i", "jpn_Jpan", "▁H", "i"],
    ["▁H", "i", "<unk>", "▁H", "i"],
    ["▁H", "i", "not-a-token", "▁H", "i"],
    ["▁H", "i", "<mask>"],
    [],
    ["こ", "ん", "▁に", "ち", "は", "。"],
]


def train_vocab(workdir: Path) -> list:
    corpus = workdir / "corpus.txt"
    corpus.write_text("\n".join(m2m.CORPUS) + "\n", encoding="utf-8")
    prefix = workdir / "nllb"
    sentencepiece.SentencePieceTrainer.train(
        input=str(corpus), model_prefix=str(prefix), vocab_size=240, model_type="unigram", character_coverage=1.0,
        hard_vocab_limit=False, unk_id=3, bos_id=0, eos_id=2, pad_id=1, user_defined_symbols=CLEAN_UP_PIECES, minloglevel=2,
    )
    processor = sentencepiece.SentencePieceProcessor(model_file=f"{prefix}.model")
    return [(processor.id_to_piece(i), processor.get_score(i)) for i in range(processor.get_piece_size())]


def build_tokenizer(vocab: list) -> Tokenizer:
    tokenizer = Tokenizer(models.Unigram(vocab, unk_id=3, byte_fallback=False))
    tokenizer.normalizer = normalizers.Sequence([normalizers.NFKC(), normalizers.Replace(" {2,}", " ")])
    tokenizer.pre_tokenizer = pre_tokenizers.Metaspace(replacement="▁", prepend_scheme="always", split=True)
    tokenizer.decoder = decoders.Metaspace(replacement="▁", prepend_scheme="always", split=True)
    tokenizer.add_special_tokens(["<s>", "<pad>", "</s>", "<unk>"])
    tokenizer.add_tokens([AddedToken(code, special=True, normalized=False) for code in LANGS])
    tokenizer.add_special_tokens(["<mask>"])
    return tokenizer


def token_list(tokenizer: Tokenizer) -> list:
    return [tokenizer.id_to_token(i) for i in range(tokenizer.get_vocab_size())]


def source_tokens(tokenizer: Tokenizer, text: str, source: str) -> list:
    """The fast tokenizer with `src_lang = source`: a post-processor template adds the code and </s>."""
    tokenizer.post_processor = processors.TemplateProcessing(
        single=f"{source} $A </s>",
        pair=f"{source} $A $B </s>",
        special_tokens=[(source, tokenizer.token_to_id(source)), ("</s>", tokenizer.token_to_id("</s>"))],
    )
    templated = tokenizer.encode(text, add_special_tokens=True).tokens
    by_hand = [source] + tokenizer.encode(text, add_special_tokens=False).tokens + ["</s>"]
    assert templated == by_hand, (templated, by_hand)
    return templated


def decode(tokenizer: Tokenizer, tokens: list) -> str:
    unk = tokenizer.token_to_id("<unk>")
    ids = [tokenizer.token_to_id(token) if tokenizer.token_to_id(token) is not None else unk for token in tokens]
    text = tokenizer.decode(ids, skip_special_tokens=False)
    # PreTrainedTokenizerBase.clean_up_tokenization (config says clean_up_tokenization_spaces: true).
    for old, new in ((" .", "."), (" ?", "?"), (" !", "!"), (" ,", ","), (" ' ", "'"), (" n't", "n't"), (" 'm", "'m"),
                     (" 's", "'s"), (" 've", "'ve"), (" 're", "'re")):
        text = text.replace(old, new)
    return text


def build_model(tokenizer: Tokenizer, workdir: Path) -> Path:
    tokens = token_list(tokenizer)
    torch.manual_seed(11)
    config = M2M100Config(
        vocab_size=len(tokens), d_model=16, encoder_layers=1, decoder_layers=1, encoder_attention_heads=2,
        decoder_attention_heads=2, encoder_ffn_dim=32, decoder_ffn_dim=32, max_position_embeddings=512,
        pad_token_id=1, bos_token_id=0, eos_token_id=2, decoder_start_token_id=2,
    )
    model = M2M100ForConditionalGeneration(config)
    with torch.no_grad():
        for parameter in model.parameters():
            if parameter.dim() > 1:
                parameter.normal_(0.0, 1.2)
    source = workdir / "hf_model"
    model.save_pretrained(source)
    # The converter wants a tokenizer next to the weights only to read the vocabulary; hand it ours.
    tokenizer.save(str(source / "tokenizer.json"))
    (source / "tokenizer_config.json").write_text(json.dumps({"tokenizer_class": "PreTrainedTokenizerFast", "bos_token": "<s>", "eos_token": "</s>", "unk_token": "<unk>", "pad_token": "<pad>"}), encoding="utf-8")
    from ctranslate2.converters import transformers as converters

    converters.M2M100Loader.get_vocabulary = lambda self, model, tokenizer: list(tokens)
    target = workdir / "ct2_model"
    ctranslate2.converters.TransformersConverter(str(source)).convert(str(target), quantization="float32", force=True)
    return target


def main() -> None:
    sys.stdout.reconfigure(encoding="utf-8")
    with tempfile.TemporaryDirectory() as scratch:
        workdir = Path(scratch)
        tokenizer = build_tokenizer(train_vocab(workdir))
        model_dir = build_model(tokenizer, workdir)

        translator = ctranslate2.Translator(str(model_dir), device="cpu", compute_type="float32", inter_threads=1, intra_threads=1)
        cases = []
        for text, source, target in CASES:
            source_list = source_tokens(tokenizer, text, source)
            hypothesis = translator.translate_batch(
                [source_list], target_prefix=[[target]], max_decoding_length=MAX_DECODING_LENGTH
            )[0].hypotheses[0]
            cases.append({
                "text": text, "source": source, "target": target, "source_tokens": source_list,
                "hypothesis": hypothesis, "output": decode(tokenizer, hypothesis[1:]),
            })
        decode_cases = [{"tokens": tokens_, "output": decode(tokenizer, tokens_)} for tokens_ in DECODE_CASES]

        if OUT.exists():
            shutil.rmtree(OUT)
        shutil.copytree(model_dir, OUT / "model")
        (OUT / "tokenizer").mkdir(parents=True)
        tokenizer.save(str(OUT / "tokenizer" / "tokenizer.json"))
        (OUT / "tokenizer" / "tokenizer_config.json").write_text(
            json.dumps({"tokenizer_class": "NllbTokenizerFast", "clean_up_tokenization_spaces": True}, indent=1) + "\n", encoding="utf-8"
        )
    document = {"max_decoding_length": MAX_DECODING_LENGTH, "cases": cases, "decode_cases": decode_cases}
    (OUT / "golden.json").write_text(json.dumps(document, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    size = sum(path.stat().st_size for path in OUT.rglob("*") if path.is_file())
    print(f"{len(cases)} cases, {len(decode_cases)} decode cases, {size / 1024:.0f} KB -> {OUT}")
    for case in cases[:3]:
        print(case["source_tokens"], "->", case["hypothesis"][:6], "->", repr(case["output"][:60]))


if __name__ == "__main__":
    sys.exit(main())
