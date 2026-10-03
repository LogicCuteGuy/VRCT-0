use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use vrct_core::transliteration::{kata_to_hira, katakana_to_hepburn, Transliterator};

fn resources() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources/transliteration")
}

fn converter() -> &'static Transliterator {
    static INSTANCE: OnceLock<Transliterator> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        Transliterator::load(resources()).expect("run the native resource preparation script first")
    })
}

#[test]
fn python_sudachipy_full_dictionary_golden_parity() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/transliteration.json")).unwrap();
    assert_eq!(cases.len(), 164);
    for case in cases {
        let text = case["text"].as_str().unwrap();
        let hira = case["hiragana"].as_bool().unwrap();
        let romaji = case["romaji"].as_bool().unwrap();
        let output = converter().transliterate(text, hira, romaji).unwrap();
        assert_eq!(
            Value::Array(output),
            case["expected"],
            "input {text:?}; hiragana {hira}; romaji {romaji}"
        );
    }
}

#[test]
fn shared_dictionary_supports_concurrent_messages() {
    let converter = Arc::new(converter().clone());
    let expected = converter.transliterate("東京に行く😀", true, true).unwrap();
    let workers: Vec<_> = (0..12)
        .map(|_| {
            let converter = converter.clone();
            std::thread::spawn(move || converter.transliterate("東京に行く😀", true, true).unwrap())
        })
        .collect();
    for worker in workers {
        assert_eq!(worker.join().unwrap(), expected);
    }
}

#[test]
fn missing_dictionary_is_an_explicit_error() {
    let error = match Transliterator::load(resources().join("does-not-exist")) {
        Ok(_) => panic!("missing dictionary must not initialize"),
        Err(error) => error,
    };
    assert!(error.contains("Sudachi dictionary"));
}

#[test]
fn malformed_dictionary_is_rejected_before_native_parse() {
    let dir = std::env::temp_dir().join(format!("vrct-sudachi-corrupt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("system.dic"), b"not a dictionary").unwrap();
    let result = Transliterator::load(&dir);
    assert!(matches!(result, Err(error) if error.contains("wrong size")));
    std::fs::remove_file(dir.join("system.dic")).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn long_input_reports_error_without_panicking_and_recovers() {
    let text = "日".repeat(40_000);
    assert!(converter().transliterate(&text, true, true).is_err());
    assert!(!converter()
        .transliterate("東京", true, true)
        .unwrap()
        .is_empty());
    assert_eq!(
        converter().transliterate(&text, false, false).unwrap(),
        Vec::<Value>::new()
    );
}

#[test]
fn kana_rules_match_original_non_macron_hepburn() {
    for (kana, expected) in [
        ("カタカナ", "katakana"),
        ("コンピューター", "kompyuutaa"),
        ("マッチャ", "maccha"),
        ("シンブン", "shimbun"),
        ("ヴァイオリン", "vaiorin"),
        ("ヮヵヶ", ""),
        ("ー--", ""),
        ("カ-ー", "kaaa"),
        ("ＡＢＣ-hello", "ａｂｃhello"),
        ("\u{1c}カ\u{1f}", "ka"),
    ] {
        assert_eq!(katakana_to_hepburn(kana), expected, "{kana}");
    }
    assert_eq!(kata_to_hira("ヴヵヶカタカナ😀"), "ヴヵヶかたかな😀");
}
