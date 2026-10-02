//! Local CTranslate2 translation against a tiny model built by
//! `tests/fixtures/regenerate_ct2_golden.py`, whose expected output is what
//! Python's own tokenizer and `ctranslate2` produce on it.
//!
//! The model is random, so its "translations" are noise; what is checked is
//! that Rust and Python agree on every step. Build with the `ct2` feature:
//!
//!     RUSTFLAGS="-C target-feature=+crt-static" CARGO_TARGET_DIR=C:/vrct_static \
//!         cargo test -p vrct-core --features ct2 --test translation_ct2
#![cfg(feature = "ct2")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use vrct_core::protocol::{parse_sidecar_line, Response};
use vrct_core::rpc::{LineWriter, Rpc, IMPLEMENTED};
use vrct_core::translation::ct2::m2m100::Tokenizer;
use vrct_core::translation::ct2::{Engine as Ct2, LoadRequest, TranslateRequest};

/// The fixture passes for this weight type; only the name matters to the loader.
const WEIGHT_TYPE: &str = "m2m100_418M-ct2-int8";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ct2_tiny")
}

fn golden() -> Value {
    serde_json::from_str(&fs::read_to_string(fixtures().join("golden.json")).unwrap()).unwrap()
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|item| item.as_str().unwrap().to_string()).collect()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A VRCT data root laid out as Python leaves it, removed when dropped.
struct Root(PathBuf);

impl Root {
    /// `snapshot` puts the tokenizer where the Hugging Face cache does.
    fn new(snapshot: bool) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!("vrct-ct2-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
        let model = root.join("weights/ctranslate2").join(WEIGHT_TYPE);
        copy_dir(&fixtures().join("model"), &model);
        let tokenizer = if snapshot {
            model.join("tokenizer/models--facebook--m2m100_418M/snapshots/0123abcd")
        } else {
            model.join("tokenizer")
        };
        copy_dir(&fixtures().join("tokenizer"), &tokenizer);
        Root(root)
    }

    fn load_request(&self) -> LoadRequest {
        serde_json::from_value(json!({"path": self.0, "weight_type": WEIGHT_TYPE, "compute_type": "float32"})).unwrap()
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn translate_request(text: &str, source: &str, target: &str, max_decoding_length: Option<usize>) -> TranslateRequest {
    serde_json::from_value(json!({
        "message": text, "source_language": source, "target_language": target,
        "weight_type": WEIGHT_TYPE, "max_decoding_length": max_decoding_length,
    }))
    .unwrap()
}

fn tokenizer() -> Tokenizer {
    Tokenizer::open(&fixtures().join("tokenizer")).unwrap()
}

#[test]
fn source_tokens_are_what_python_feeds_the_model() {
    let tokenizer = tokenizer();
    let golden = golden();
    for case in golden["cases"].as_array().unwrap() {
        let got = tokenizer
            .source_tokens(case["text"].as_str().unwrap(), case["source"].as_str().unwrap())
            .unwrap();
        assert_eq!(got, strings(&case["source_tokens"]), "{}", case["text"]);
    }
}

#[test]
fn decoding_matches_the_python_tokenizer() {
    let tokenizer = tokenizer();
    let golden = golden();
    let cases = golden["decode_cases"].as_array().unwrap();
    assert!(cases.len() > 60, "the corpus should include the random runs");
    for case in cases {
        let tokens = strings(&case["tokens"]);
        assert_eq!(tokenizer.decode(&tokens).unwrap(), case["output"].as_str().unwrap(), "{tokens:?}");
    }
}

#[test]
fn the_language_table_is_the_one_python_has() {
    let golden = golden();
    let languages = strings(&golden["languages"]);
    assert_eq!(languages.len(), 100);
    assert!(languages.iter().all(|code| Tokenizer::knows(code)));
    assert_eq!(Tokenizer::languages().len(), languages.len());
    assert!(!Tokenizer::knows("xx") && !Tokenizer::knows("") && !Tokenizer::knows("JA"));
    assert_eq!(tokenizer().target_prefix("ja").unwrap(), "__ja__");
    assert!(tokenizer().target_prefix("klingon").is_err());
    assert!(tokenizer().source_tokens("hi", "klingon").is_err());
}

#[test]
fn clean_up_follows_the_tokenizer_config() {
    let with = tokenizer();
    let tokens = strings(&json!(["▁H", "i", "▁!"]));
    assert_eq!(with.decode(&tokens).unwrap(), "Hi!");

    // `clean_up_tokenization_spaces: false` keeps the space before the mark.
    let root = Root::new(false);
    let tokenizer_dir = root.0.join("weights/ctranslate2").join(WEIGHT_TYPE).join("tokenizer");
    fs::write(tokenizer_dir.join("tokenizer_config.json"), r#"{"clean_up_tokenization_spaces": false}"#).unwrap();
    let without = Tokenizer::open(&tokenizer_dir).unwrap();
    assert_eq!(without.decode(&tokens).unwrap(), "Hi !");
    // No config file at all: transformers 4.x cleans up.
    fs::remove_file(tokenizer_dir.join("tokenizer_config.json")).unwrap();
    assert_eq!(Tokenizer::open(&tokenizer_dir).unwrap().decode(&tokens).unwrap(), "Hi!");
}

#[test]
fn the_tokenizer_is_found_where_the_hugging_face_cache_puts_it() {
    for snapshot in [false, true] {
        let root = Root::new(snapshot);
        let tokenizer_root = root.0.join("weights/ctranslate2").join(WEIGHT_TYPE).join("tokenizer");
        let found = Tokenizer::find(&tokenizer_root).expect("tokenizer files should be found");
        assert!(found.join("sentencepiece.bpe.model").is_file());
        assert!(Tokenizer::open(&found).is_ok());
    }
    let empty = Root::new(false);
    let nowhere = empty.0.join("weights/ctranslate2/none");
    assert!(Tokenizer::find(&nowhere).is_none());
}

#[test]
fn translations_match_python_token_for_token() {
    let root = Root::new(true);
    let engine = Ct2::default();
    engine.load(&root.load_request()).unwrap();
    assert!(engine.is_loaded(WEIGHT_TYPE));

    let golden = golden();
    let length = golden["max_decoding_length"].as_u64().unwrap() as usize;
    let mut compared = 0;
    for case in golden["cases"].as_array().unwrap() {
        let request = translate_request(
            case["text"].as_str().unwrap(),
            case["source"].as_str().unwrap(),
            case["target"].as_str().unwrap(),
            Some(length),
        );
        assert_eq!(engine.translate(&request).unwrap(), case["output"].as_str().unwrap(), "{}", case["text"]);
        compared += 1;
    }
    assert!(compared >= 16);
}

#[test]
fn a_failed_or_unsupported_load_leaves_nothing_loaded() {
    let root = Root::new(false);
    let engine = Ct2::default();
    engine.load(&root.load_request()).unwrap();

    let mut cuda = root.load_request();
    cuda.device = "cuda".into();
    assert!(engine.load(&cuda).unwrap_err().contains("CPU only"));
    assert!(!engine.is_loaded(WEIGHT_TYPE), "a refused load drops the previous model, as Python does");

    let mut nllb = root.load_request();
    nllb.weight_type = "nllb-200-distilled-600M-ct2-int8".into();
    assert!(engine.load(&nllb).unwrap_err().contains("not run by this build"));

    let mut odd = root.load_request();
    odd.compute_type = "int3".into();
    assert!(engine.load(&odd).unwrap_err().contains("unknown compute type"));

    // The weights are there but the tokenizer is not.
    let model = root.0.join("weights/ctranslate2").join(WEIGHT_TYPE);
    fs::remove_dir_all(model.join("tokenizer")).unwrap();
    assert!(engine.load(&root.load_request()).unwrap_err().contains("no tokenizer files"));

    // The tokenizer is there but the weights are not.
    let bare = Root::new(false);
    fs::remove_file(bare.0.join("weights/ctranslate2").join(WEIGHT_TYPE).join("model.bin")).unwrap();
    assert!(engine.load(&bare.load_request()).unwrap_err().starts_with("cannot load the model"));
    assert!(!engine.is_loaded(WEIGHT_TYPE));
}

#[test]
fn translating_without_the_right_model_is_an_error() {
    let engine = Ct2::default();
    assert_eq!(engine.translate(&translate_request("hi", "en", "ja", Some(4))).unwrap_err(), "no model is loaded");

    let root = Root::new(false);
    engine.load(&root.load_request()).unwrap();
    let mut other = translate_request("hi", "en", "ja", Some(4));
    other.weight_type = "m2m100_1.2B-ct2-int8".into();
    assert!(engine.translate(&other).unwrap_err().contains("is loaded, not"));
    assert!(engine.translate(&translate_request("hi", "en", "klingon", Some(4))).unwrap_err().contains("unknown language"));
    assert!(engine.translate(&translate_request("hi", "klingon", "ja", Some(4))).unwrap_err().contains("unknown language"));
}

#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl LineWriter for Lines {
    fn write_line(&self, line: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(line.to_string());
        Ok(())
    }
}

impl Lines {
    async fn wait_for(&self, count: usize) -> Vec<Value> {
        for _ in 0..800 {
            if self.0.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|line| {
                let envelope: Value = serde_json::from_str(line.trim_end()).unwrap();
                serde_json::from_slice(&STANDARD.decode(envelope["data"].as_str().unwrap()).unwrap()).unwrap()
            })
            .collect()
    }
}

fn request_line(result: Value) -> Response {
    let text = json!({"status": 200, "endpoint": "/internal/rpc/request", "result": result}).to_string();
    parse_sidecar_line(&text).unwrap()
}

#[tokio::test]
async fn the_model_is_loaded_and_used_through_the_call_bridge() {
    assert!(IMPLEMENTED.contains(&"ct2.load") && IMPLEMENTED.contains(&"ct2.translate"));

    let root = Root::new(true);
    let lines = Arc::new(Lines::default());
    let rpc = Rpc::new(lines.clone());
    let golden = golden();
    let case = &golden["cases"][0];

    // Nothing loaded yet: an error answer, not a hang.
    let params = json!({
        "message": case["text"], "source_language": case["source"], "target_language": case["target"],
        "weight_type": WEIGHT_TYPE, "max_decoding_length": golden["max_decoding_length"],
    });
    assert!(rpc.ingest(&request_line(json!({"id": 1, "method": "ct2.translate", "params": params}))));
    let answers = lines.wait_for(1).await;
    assert_eq!(answers[0]["ok"], false);

    let load = json!({"path": root.0, "weight_type": WEIGHT_TYPE, "device": "cpu", "device_index": 0, "compute_type": "float32"});
    assert!(rpc.ingest(&request_line(json!({"id": 2, "method": "ct2.load", "params": load}))));
    let answers = lines.wait_for(2).await;
    assert_eq!(answers[1], json!({"id": 2, "ok": true, "result": true}));

    assert!(rpc.ingest(&request_line(json!({"id": 3, "method": "ct2.translate", "params": params}))));
    let answers = lines.wait_for(3).await;
    assert_eq!(answers[2], json!({"id": 3, "ok": true, "result": case["output"]}));
}
