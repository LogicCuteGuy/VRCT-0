//! Real `jncraton/m2m100_418M-ct2-int8` translation replays historical Python
//! `translateCTranslate2` output in `fixtures/ct2_m2m100_real_golden.json`.
//! Frozen at `16cb286c`; provenance and native test commands: `fixtures/README.md`.
//!
//! Requires `ct2` and `VRCT_M2M100_MODEL` (or `~/Downloads/m2m100_418M-ct2-int8`).
//! Missing/mismatched weights are reported and skipped. One test reports from
//! the inference thread and exits the process, avoiding CT2 thread-exit hangs.

#![cfg(feature = "ct2")]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use vrct_core::translation::ct2::m2m100::Tokenizer;
use vrct_core::translation::ct2::{Engine, LoadRequest, TranslateRequest};

const WEIGHT_TYPE: &str = "m2m100_418M-ct2-int8";

fn golden() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ct2_m2m100_real_golden.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn model_dir() -> Option<PathBuf> {
    if let Some(given) = std::env::var_os("VRCT_M2M100_MODEL") {
        return Some(PathBuf::from(given));
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let dir = PathBuf::from(home).join("Downloads").join("m2m100_418M-ct2-int8");
    dir.join("model.bin").exists().then_some(dir)
}

fn sha256(path: &Path) -> String {
    let mut digest = Sha256::new();
    std::io::copy(&mut fs::File::open(path).unwrap(), &mut digest).unwrap();
    hex::encode(digest.finalize())
}

/// A VRCT data root laid out as Python leaves it (`weights/ctranslate2/<type>/` with the tokenizer files in
/// its `tokenizer/` folder), made from the model folder and removed when dropped.
struct Root(PathBuf);

impl Root {
    fn from(model: &Path) -> Self {
        let root = std::env::temp_dir().join(format!("vrct-ct2-real-{}", std::process::id()));
        let weights = root.join("weights/ctranslate2").join(WEIGHT_TYPE);
        fs::create_dir_all(weights.join("tokenizer")).unwrap();
        for name in ["model.bin", "config.json", "shared_vocabulary.json"] {
            fs::copy(model.join(name), weights.join(name)).unwrap();
        }
        for name in ["sentencepiece.bpe.model", "vocab.json", "tokenizer_config.json"] {
            fs::copy(model.join(name), weights.join("tokenizer").join(name)).unwrap();
        }
        Root(root)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Translates every golden case with `compute_type`; the cases whose text is not `expected[i]`, described.
fn differences(engine: &Engine, root: &Root, compute_type: &str, expected: &[String]) -> Vec<String> {
    engine
        .load(&LoadRequest { path: root.0.clone(), weight_type: WEIGHT_TYPE.into(), device: "cpu".into(), device_index: 0, compute_type: compute_type.into() })
        .expect("the model loads");
    let mut different = Vec::new();
    for (case, want) in golden()["cases"].as_array().unwrap().iter().zip(expected) {
        let (text, source, target) = (case["text"].as_str().unwrap(), case["source"].as_str().unwrap(), case["target"].as_str().unwrap());
        let got = engine
            .translate(&TranslateRequest {
                message: text.into(),
                source_language: source.into(),
                target_language: target.into(),
                weight_type: WEIGHT_TYPE.into(),
                max_decoding_length: None,
            })
            .unwrap_or_else(|e| panic!("{source}->{target} {text:?}: {e}"));
        if &got != want {
            different.push(format!("{source}->{target} {text:?}
   python: {want:?}
   rust:   {got:?}"));
        }
    }
    different
}

fn compare(model: &Path) {
    let golden = golden();
    let cases = golden["cases"].as_array().unwrap();

    // The tokenizer: every source token list is Python's.
    let tokenizer = Tokenizer::open(model).expect("the tokenizer opens");
    for case in cases {
        let (text, source) = (case["text"].as_str().unwrap(), case["source"].as_str().unwrap());
        let tokens: Vec<String> = case["source_tokens"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
        assert_eq!(tokens, tokenizer.source_tokens(text, source).unwrap(), "{source} {text:?}: source tokens");
    }

    let root = Root::from(model);
    let engine = Engine::default();

    // float32 has no int8 kernels to differ in: every translation is Python's.
    let exact: Vec<String> = golden["float32_outputs"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
    let float32 = differences(&engine, &root, "float32", &exact);
    eprintln!("float32: {} of {} differ from Python", float32.len(), cases.len());
    for line in &float32 {
        eprintln!("{line}");
    }

    // int8 (what VRCT runs): ruy here, MKL in Python. Near ties in the beam can go either way: Python itself
    // changes 10 of these 38 sentences between int8 and float32. Most must still agree.
    let auto: Vec<String> = cases.iter().map(|c| c["output"].as_str().unwrap().to_string()).collect();
    let int8 = differences(&engine, &root, "auto", &auto);
    eprintln!("auto (int8): {} of {} differ from Python", int8.len(), cases.len());
    for line in &int8 {
        eprintln!("{line}");
    }

    assert!(float32.is_empty(), "{} float32 translations differ from Python's", float32.len());
    assert!(int8.len() * 100 <= cases.len() * 30, "{} of {} int8 translations differ from Python's", int8.len(), cases.len());
}

#[test]
fn the_real_m2m100_translates_as_python_does() {
    let Some(model) = model_dir() else { return eprintln!("skipped: no m2m100_418M-ct2-int8 folder here") };
    let golden = golden();
    if fs::metadata(model.join("model.bin")).map(|m| m.len()).ok() != golden["model_bin_size"].as_u64()
        || sha256(&model.join("model.bin")) != golden["model_bin_sha256"].as_str().unwrap()
    {
        return eprintln!("skipped: this model.bin is not the one the golden was made from");
    }
    for (name, sha) in golden["tokenizer_files"].as_object().unwrap() {
        if sha256(&model.join(name)) != sha.as_str().unwrap() {
            return eprintln!("skipped: {name} is not the one the golden was made from");
        }
    }
    let outcome = std::panic::catch_unwind(|| compare(&model));
    match outcome {
        Ok(()) => {
            eprintln!("the_real_m2m100_translates_as_python_does ... ok");
            std::process::exit(0)
        }
        Err(_) => std::process::exit(101),
    }
}
