//! Japanese reading conversion compatible with VRCT's former SudachiPy pipeline.
//!
//! The dictionary is native Sudachi full 20250825; no Python process or extension
//! is loaded. Keep its resources and license beside the application. A dictionary
//! is immutable and shared; each call owns its tokenizer so simultaneous mic,
//! speaker and chat messages cannot borrow the same tokenizer concurrently.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path, sync::Arc};
use sudachi::{
    analysis::{stateful_tokenizer::StatefulTokenizer, Mode},
    config::Config,
    dic::dictionary::JapaneseDictionary,
    prelude::MorphemeList,
};

pub const DICTIONARY_SHA256: &str =
    "08eac653b59c4ad579e38fcf64ce338fd3edab87c28a1ad97449f5fe2a9bcf87";
pub const DICTIONARY_BYTES: u64 = 359_725_440;

#[derive(Clone)]
pub struct Transliterator {
    dictionary: Arc<JapaneseDictionary>,
}

impl Transliterator {
    /// Validate and load the packaged dictionary. Missing/corrupt resources are
    /// errors, rather than silently dropping annotations on enabled features.
    pub fn load(resources: impl AsRef<Path>) -> Result<Self, String> {
        let resources = resources.as_ref();
        let dictionary = resources.join("system.dic");
        validate_dictionary(&dictionary)?;
        let config = Config::new(
            Some(resources.join("sudachi.json")),
            Some(resources.to_owned()),
            Some(dictionary),
        )
        .map_err(|error| format!("Sudachi config: {error}"))?;
        let dictionary = JapaneseDictionary::from_cfg(&config)
            .map_err(|error| format!("Sudachi dictionary: {error}"))?;
        Ok(Self {
            dictionary: Arc::new(dictionary),
        })
    }

    pub fn transliterate(
        &self,
        message: &str,
        hiragana: bool,
        romaji: bool,
    ) -> Result<Vec<Value>, String> {
        if (!hiragana && !romaji) || message.is_empty() {
            return Ok(Vec::new());
        }
        let mut tokenizer = StatefulTokenizer::new(self.dictionary.clone(), Mode::C);
        tokenizer.reset().push_str(message);
        tokenizer
            .do_tokenize()
            .map_err(|error| format!("Sudachi tokenize: {error}"))?;
        let mut morphemes = MorphemeList::empty(self.dictionary.clone());
        morphemes
            .collect_results(&mut tokenizer)
            .map_err(|error| format!("Sudachi results: {error}"))?;
        let mut parts = Vec::new();
        for token in morphemes.iter() {
            let surface = token.surface().to_string();
            let pos = token.part_of_speech().first().map(String::as_str);
            let reading = if matches!(pos, Some("記号" | "補助記号" | "空白")) {
                surface.as_str()
            } else {
                token.reading_form()
            };
            if surface == reading || surface.chars().count() == 1 {
                parts.push((surface.clone(), reading.to_owned()));
            } else {
                parts.extend(split_kanji_okurigana(&surface, reading));
            }
        }
        // The former single embedded contextual rule: 何 is ナン before
        // タ/ダ/ナ-row syllables, otherwise ナニ. Inspect the next sub-unit,
        // including punctuation/space, exactly as the source does.
        for index in 0..parts.len() {
            if parts[index].0 == "何" {
                let next = parts[index + 1..].iter().find(|(orig, _)| !orig.is_empty());
                let first = next.and_then(|(orig, kana)| {
                    kana.chars()
                        .next()
                        .or_else(|| orig.chars().next().filter(|c| ('ァ'..='ン').contains(c)))
                });
                parts[index].1 = if first
                    .is_some_and(|c| "タチツテトダヂヅデドナニヌネノ".contains(c))
                {
                    "ナン"
                } else {
                    "ナニ"
                }
                .to_owned();
            }
        }
        Ok(parts
            .into_iter()
            .map(|(orig, kana)| {
                let mut entry = Map::new();
                entry.insert("orig".into(), Value::String(orig.clone()));
                if hiragana {
                    entry.insert("hira".into(), Value::String(kata_to_hira(&kana)));
                }
                if romaji {
                    entry.insert("hepburn".into(), Value::String(katakana_to_hepburn(&kana)));
                }
                Value::Object(entry)
            })
            .collect())
    }
}

fn validate_dictionary(path: &Path) -> Result<(), String> {
    let mut file = File::open(path)
        .map_err(|error| format!("Sudachi dictionary {}: {error}", path.display()))?;
    if file.metadata().map_err(|error| error.to_string())?.len() != DICTIONARY_BYTES {
        return Err("Sudachi dictionary has the wrong size (expected full 20250825)".into());
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if hex::encode(hasher.finalize()) != DICTIONARY_SHA256 {
        return Err("Sudachi dictionary checksum mismatch (expected full 20250825)".into());
    }
    Ok(())
}

fn is_kanji(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

pub fn kata_to_hira(text: &str) -> String {
    text.chars()
        .map(|c| {
            if ('ァ'..='ン').contains(&c) {
                char::from_u32(c as u32 - 0x60).expect("hiragana codepoint")
            } else {
                c
            }
        })
        .collect()
}

/// Preserve the former heuristic's allocation, including its uncommon short
/// reading behavior. Use Unicode scalar counts, never UTF-8 byte lengths.
fn split_kanji_okurigana(surface: &str, reading: &str) -> Vec<(String, String)> {
    let mut blocks: Vec<(bool, String)> = Vec::new();
    for c in surface.chars() {
        let kanji = is_kanji(c);
        if let Some((previous, text)) = blocks.last_mut() {
            if *previous == kanji {
                text.push(c);
                continue;
            }
        }
        blocks.push((kanji, c.to_string()));
    }
    let kana: Vec<char> = reading.chars().collect();
    let mut allocations: Vec<usize> = blocks
        .iter()
        .map(|(_, part)| part.chars().count())
        .collect();
    let allocated: usize = allocations.iter().sum();
    if kana.len() > allocated {
        let mut remaining = kana.len() - allocated;
        for (index, (kanji, _)) in blocks.iter().enumerate() {
            if remaining == 0 {
                break;
            }
            if *kanji {
                allocations[index] += 1;
                remaining -= 1;
            }
        }
        if !blocks.is_empty() {
            for index in 0..remaining {
                allocations[index % blocks.len()] += 1;
            }
        }
    } else if kana.len() < allocated {
        let mut need = allocated - kana.len();
        for amount in allocations.iter_mut().rev() {
            let take = amount.saturating_sub(1).min(need);
            *amount -= take;
            need -= take;
            if need == 0 {
                break;
            }
        }
    }
    let mut pos = 0usize;
    blocks
        .into_iter()
        .zip(allocations)
        .map(|((_, part), count)| {
            let begin = pos.min(kana.len());
            pos += count;
            (part, kana[begin..pos.min(kana.len())].iter().collect())
        })
        .collect()
}

fn base(c: char) -> Option<&'static str> {
    Some(match c {
        'ア' | 'ァ' => "a",
        'イ' | 'ィ' => "i",
        'ウ' | 'ゥ' => "u",
        'エ' | 'ェ' => "e",
        'オ' | 'ォ' => "o",
        'カ' => "ka",
        'キ' => "ki",
        'ク' => "ku",
        'ケ' => "ke",
        'コ' => "ko",
        'サ' => "sa",
        'シ' => "shi",
        'ス' => "su",
        'セ' => "se",
        'ソ' => "so",
        'タ' => "ta",
        'チ' => "chi",
        'ツ' => "tsu",
        'テ' => "te",
        'ト' => "to",
        'ナ' => "na",
        'ニ' => "ni",
        'ヌ' => "nu",
        'ネ' => "ne",
        'ノ' => "no",
        'ハ' => "ha",
        'ヒ' => "hi",
        'フ' => "fu",
        'ヘ' => "he",
        'ホ' => "ho",
        'マ' => "ma",
        'ミ' => "mi",
        'ム' => "mu",
        'メ' => "me",
        'モ' => "mo",
        'ヤ' | 'ャ' => "ya",
        'ユ' | 'ュ' => "yu",
        'ヨ' | 'ョ' => "yo",
        'ラ' => "ra",
        'リ' => "ri",
        'ル' => "ru",
        'レ' => "re",
        'ロ' => "ro",
        'ワ' => "wa",
        'ヲ' => "wo",
        'ン' => "n",
        'ガ' => "ga",
        'ギ' => "gi",
        'グ' => "gu",
        'ゲ' => "ge",
        'ゴ' => "go",
        'ザ' => "za",
        'ジ' | 'ヂ' => "ji",
        'ズ' | 'ヅ' => "zu",
        'ゼ' => "ze",
        'ゾ' => "zo",
        'ダ' => "da",
        'デ' => "de",
        'ド' => "do",
        'バ' => "ba",
        'ビ' => "bi",
        'ブ' => "bu",
        'ベ' => "be",
        'ボ' => "bo",
        'パ' => "pa",
        'ピ' => "pi",
        'プ' => "pu",
        'ペ' => "pe",
        'ポ' => "po",
        'ヴ' => "vu",
        'ッ' => "xtsu",
        'ー' => "-",
        _ => return None,
    })
}

fn digraph(a: char, b: char) -> Option<&'static str> {
    Some(match (a, b) {
        ('キ', 'ャ') => "kya",
        ('キ', 'ュ') => "kyu",
        ('キ', 'ョ') => "kyo",
        ('ギ', 'ャ') => "gya",
        ('ギ', 'ュ') => "gyu",
        ('ギ', 'ョ') => "gyo",
        ('シ', 'ャ') => "sha",
        ('シ', 'ュ') => "shu",
        ('シ', 'ョ') => "sho",
        ('ジ', 'ャ') => "ja",
        ('ジ', 'ュ') => "ju",
        ('ジ', 'ョ') => "jo",
        ('チ', 'ャ') => "cha",
        ('チ', 'ュ') => "chu",
        ('チ', 'ョ') => "cho",
        ('ニ', 'ャ') => "nya",
        ('ニ', 'ュ') => "nyu",
        ('ニ', 'ョ') => "nyo",
        ('ヒ', 'ャ') => "hya",
        ('ヒ', 'ュ') => "hyu",
        ('ヒ', 'ョ') => "hyo",
        ('ビ', 'ャ') => "bya",
        ('ビ', 'ュ') => "byu",
        ('ビ', 'ョ') => "byo",
        ('ピ', 'ャ') => "pya",
        ('ピ', 'ュ') => "pyu",
        ('ピ', 'ョ') => "pyo",
        ('ミ', 'ャ') => "mya",
        ('ミ', 'ュ') => "myu",
        ('ミ', 'ョ') => "myo",
        ('リ', 'ャ') => "rya",
        ('リ', 'ュ') => "ryu",
        ('リ', 'ョ') => "ryo",
        ('フ', 'ャ') => "fya",
        ('フ', 'ュ') => "fyu",
        ('フ', 'ョ') => "fyo",
        ('ト', 'ゥ') => "tu",
        ('ド', 'ゥ') => "du",
        ('フ', 'ァ') => "fa",
        ('フ', 'ィ') => "fi",
        ('フ', 'ェ') => "fe",
        ('フ', 'ォ') => "fo",
        ('シ', 'ェ') => "she",
        ('チ', 'ェ') => "che",
        ('テ', 'ィ') => "ti",
        ('ウ', 'ァ') => "wa",
        ('ウ', 'ィ') => "wi",
        ('ウ', 'ェ') => "we",
        ('ウ', 'ォ') => "wo",
        ('ス', 'ィ') => "si",
        ('ズ', 'ィ') => "zi",
        ('ツ', 'ァ') => "tsa",
        ('ツ', 'ィ') => "tsi",
        ('ツ', 'ェ') => "tse",
        ('ツ', 'ォ') => "tso",
        ('キ', 'ェ') => "kye",
        ('ギ', 'ェ') => "gye",
        ('ヴ', 'ァ') => "va",
        ('ヴ', 'ィ') => "vi",
        ('ヴ', 'ェ') => "ve",
        ('ヴ', 'ォ') => "vo",
        ('ヴ', 'ュ') => "vyu",
        _ => return None,
    })
}

/// Hepburn rules used by the old runtime, with macrons disabled. Non-kana
/// text retains its script; ASCII is lowercased as in the former converter.
pub fn katakana_to_hepburn(text: &str) -> String {
    // Python str.strip also removes the four C0 information separators,
    // which Rust's Unicode White_Space property intentionally excludes.
    let chars: Vec<char> = text
        .trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        .chars()
        .collect();
    let mut raw = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == 'ッ' {
            let next = chars.get(i + 1).and_then(|&a| {
                chars
                    .get(i + 2)
                    .and_then(|&b| digraph(a, b))
                    .or_else(|| base(a))
            });
            if let Some(next) = next {
                if let Some(first) = next.chars().next().filter(|c| !"aeiou".contains(*c)) {
                    raw.push(first);
                }
            }
        } else if c == 'ー' {
            raw.push('-');
        } else if let Some(pair) = chars.get(i + 1).and_then(|&b| digraph(c, b)) {
            raw.push_str(pair);
            i += 1;
        } else if "ャュョァィゥェォヮヵヶ".contains(c) {
            raw.push_str(base(c).unwrap_or(""));
        } else if let Some(roman) = base(c) {
            raw.push_str(roman);
        } else {
            raw.push(c);
        }
        i += 1;
    }
    // Apply n -> m BEFORE prolonged marks and lowercasing, matching Python.
    let raw_chars: Vec<char> = raw.chars().collect();
    let mut extended = String::new();
    for (i, &c) in raw_chars.iter().enumerate() {
        if c == 'n' && raw_chars.get(i + 1).is_some_and(|c| "bmp".contains(*c)) {
            extended.push('m');
        } else if c == '-' {
            if let Some(vowel) = extended
                .chars()
                .next_back()
                .filter(|c| "aiueo".contains(*c))
            {
                extended.push(vowel);
            }
        } else {
            extended.push(c);
        }
    }
    extended.to_lowercase()
}
