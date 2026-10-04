//! Cloud speech-to-text engines replay the historical Python provider contract:
//! Deepgram language choice, requests/replies, model lists and WAV headers.
//! `fixtures/cloud_stt_golden.json` is frozen at `16cb286c`; see `fixtures/README.md`.
//!
//! Local scripted servers represent Python client failures with HTTP statuses,
//! refused connections or replies that never arrive.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{closed_port, hang, mock};
use serde_json::Value;
use sha2::{Digest, Sha256};
use vrct_core::transcription::clip::{wav, wav_for_upload};
use vrct_core::transcription::cloud::{Blocking, CloudRecognizer};
use vrct_core::transcription::deepgram::{self, DeepgramProvider};
use vrct_core::transcription::languages;
use vrct_core::transcription::openai::{self, OpenAiCompatible};
use vrct_core::transcription::phrases::{Format, Recognition, RecognizeError, Recognizer, Request};

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cloud_stt_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// byte i = (seed + 7 * i) % 251, as the generator makes it.
fn audio_bytes(len: usize, seed: u64) -> Vec<u8> {
    (0..len).map(|i| ((seed + 7 * (i as u64 % 251)) % 251) as u8).collect()
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

const MONO_16K: Format = Format { sample_rate: 16000, sample_width: 2, channels: 1 };

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap_or_default().to_string()).collect()
}

/// What the generator recorded as `result`, against what the engine returned.
fn assert_result(label: &str, expected: &Value, got: &Result<Recognition, RecognizeError>) {
    match (expected.get("ok"), expected.get("api_error"), expected.get("other"), got) {
        (Some(ok), _, _, Ok(found)) => {
            assert_eq!(ok["text"].as_str().unwrap(), found.text, "{label}: text");
            assert_eq!(ok["confidence"].as_f64().unwrap(), found.confidence, "{label}: confidence");
            assert_eq!(ok["definitive"].as_bool().unwrap(), found.definitive, "{label}: definitive");
        }
        (_, Some(code), _, Err(RecognizeError::Api { code: got })) => assert_eq!(code.as_str().unwrap(), got, "{label}"),
        (_, _, Some(kind), Err(RecognizeError::Other { kind: got })) => assert_eq!(kind.as_str().unwrap(), got, "{label}"),
        _ => panic!("{label}: Python gave {expected}, Rust gave {got:?}"),
    }
}

fn query_of(request_line: &str) -> Vec<(String, String)> {
    let target = request_line.split(' ').nth(1).unwrap_or_default();
    let Some((_, query)) = target.split_once('?') else { return Vec::new() };
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---- the language table and Deepgram's choice from it ----

#[test]
fn the_language_table_is_python_s() {
    assert_eq!(69, languages::languages().count());
    assert_eq!(135, languages::languages().map(|(_, countries)| countries.len()).sum::<usize>());
    assert_eq!(Some("af-ZA"), languages::code("Afrikaans", "South Africa", "Google"));
    assert_eq!(Some("af"), languages::code("Afrikaans", "South Africa", "Whisper"));
    assert_eq!(None, languages::code("Afrikaans", "Albania", "Whisper"));
    assert_eq!(None, languages::code("Afrikaans", "South Africa", "Nope"));
}

#[test]
fn deepgram_resolves_language_codes_as_python_does() {
    let golden = golden();
    let rows = golden["resolve"].as_array().unwrap();
    assert!(rows.len() > 1700);
    let mut resolved = 0;
    for row in rows {
        let (language, country) = (row["language"].as_str().unwrap(), row["country"].as_str().unwrap());
        let models = strings(&row["model_languages"]);
        let got = deepgram::resolve_language_code(language, country, &models);
        assert_eq!(row["resolved"].as_str(), got.as_deref(), "{language}/{country} with {models:?}");
        assert_eq!(
            row["supported"].as_bool().unwrap(),
            deepgram::is_language_supported(language, country, &models),
            "{language}/{country} supported with {models:?}"
        );
        resolved += usize::from(got.is_some());
    }
    assert!(resolved > 300, "the rows should include many that resolve");
}

#[test]
fn the_wav_header_is_the_one_python_s_wave_module_writes() {
    let golden = golden();
    for row in golden["wav"].as_array().unwrap() {
        let (frames, rate) = (row["frames"].as_u64().unwrap() as usize, row["rate"].as_u64().unwrap() as u32);
        let (width, channels) = (row["width"].as_u64().unwrap() as u16, row["channels"].as_u64().unwrap() as u16);
        let pcm = audio_bytes(frames * width as usize * channels as usize, 9);
        let made = wav(&pcm, rate, width, channels);
        assert_eq!(row["total"].as_u64().unwrap() as usize, made.len(), "{row}");
        assert_eq!(row["header"].as_str().unwrap(), hex::encode(&made[..44]), "{row}");
        assert_eq!(pcm, made[44..], "{row}");
    }
}

#[test]
fn a_clip_is_uploaded_as_16_khz_mono_16_bit() {
    // Already in that format: only the header is added.
    let pcm = audio_bytes(320, 1);
    assert_eq!(wav(&pcm, 16000, 2, 1), wav_for_upload(&pcm, MONO_16K).unwrap());
    // 8 kHz is brought up to 16 kHz: twice the samples, same duration.
    let slow = Format { sample_rate: 8000, sample_width: 2, channels: 1 };
    let made = wav_for_upload(&audio_bytes(160, 2), slow).unwrap();
    // 80 samples in; the resampler holds one back, as `audioop.ratecv` does.
    assert_eq!(44 + 318, made.len());
    // A stereo clip becomes mono.
    let stereo = Format { sample_rate: 16000, sample_width: 2, channels: 2 };
    assert_eq!(44 + 160, wav_for_upload(&audio_bytes(320, 3), stereo).unwrap().len());
}

// ---- Deepgram ----

#[tokio::test(flavor = "multi_thread")]
async fn deepgram_asks_and_reads_as_python_did() {
    let golden = golden();
    let cases = golden["deepgram"].as_array().unwrap();
    assert!(cases.len() >= 30);
    for case in cases {
        let label = format!("{} ({}/{} force={})", case["script"], case["language"], case["country"], case["force_language"]);
        let wait = if case["raises"] == "connection" { Duration::from_secs(15) } else { Duration::from_millis(400) };
        let (base, server) = match (case["reply"].as_object(), case["raises"].as_str()) {
            (Some(reply), _) => {
                let status = reply["status"].as_u64().unwrap() as u16;
                let server = mock(vec![(status, reply["body"].to_string())]).await;
                (server.base(), Some(server))
            }
            (None, Some("timeout")) => (format!("http://127.0.0.1:{}", hang().await), None),
            (None, _) => (format!("http://127.0.0.1:{}", closed_port()), None),
        };
        let provider = DeepgramProvider::new("KEY-1", case["model"].as_str().unwrap(), strings(&case["model_languages"]))
            .with_base_url(&base)
            .with_wait(wait);
        let pcm = audio_bytes(3200, 3);
        let request = Request {
            pcm: &pcm,
            format: MONO_16K,
            language: case["language"].as_str().unwrap(),
            country: case["country"].as_str().unwrap(),
            avg_logprob: -0.8,
            no_speech_prob: 0.6,
            no_repeat_ngram_size: 0,
            force_language: case["force_language"].as_bool().unwrap(),
        };
        let got = provider.recognize(&request).await;
        assert_result(&label, &case["result"], &got);

        let Some(server) = server else { continue };
        let sent = &server.requests()[0];
        let expected = &case["request"];
        assert!(sent.request_line.starts_with("POST /listen?"), "{label}: {}", sent.request_line);
        let params: Vec<(String, String)> = expected["params"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        let (mut params, mut sent_params) = (params, query_of(&sent.request_line));
        params.sort();
        sent_params.sort();
        assert_eq!(params, sent_params, "{label}: query");
        assert_eq!(Some("Token KEY-1"), sent.header("authorization"), "{label}");
        assert_eq!(Some("audio/wav"), sent.header("content-type"), "{label}");
        assert_eq!(expected["body_len"].as_u64().unwrap() as usize + 44, sent.raw.len(), "{label}: body size");
        assert_eq!(expected["body_sha"].as_str().unwrap(), sha(&sent.raw[44..]), "{label}: audio");
        assert_eq!(&sent.raw[..4], b"RIFF", "{label}");
        assert_eq!(1, server.requests().len(), "{label}: a Deepgram request is not retried");
    }
}

// ---- OpenAI-compatible ----

/// The named fields of a multipart body, and the file's name, type and content.
struct Multipart {
    fields: Vec<(String, String)>,
    file_name: String,
    file_type: String,
    file: Vec<u8>,
}

fn parse_multipart(content_type: &str, body: &[u8]) -> Multipart {
    let boundary = content_type.split("boundary=").nth(1).expect("a boundary").to_string();
    let delimiter = format!("--{boundary}");
    let text = body;
    let mut parts = Vec::new();
    let mut at = 0;
    let find = |from: usize, needle: &[u8]| text[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from);
    while let Some(start) = find(at, delimiter.as_bytes()) {
        let after = start + delimiter.len();
        if text[after..].starts_with(b"--") {
            break;
        }
        let head_start = after + 2; // CRLF
        let head_end = find(head_start, b"\r\n\r\n").unwrap();
        let data_start = head_end + 4;
        let next = find(data_start, delimiter.as_bytes()).unwrap();
        parts.push((String::from_utf8_lossy(&text[head_start..head_end]).to_string(), text[data_start..next - 2].to_vec()));
        at = next;
    }
    let mut fields = Vec::new();
    let (mut file_name, mut file_type, mut file) = (String::new(), String::new(), Vec::new());
    for (head, data) in parts {
        let name = head.split("name=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
        if head.contains("filename=\"") {
            file_name = head.split("filename=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
            file_type = head.split("Content-Type: ").nth(1).unwrap().trim().to_string();
            file = data;
        } else {
            fields.push((name, String::from_utf8(data).unwrap()));
        }
    }
    Multipart { fields, file_name, file_type, file }
}

/// The scripted failure Python's client raised, as something a server can do.
enum Failure {
    Status(u16),
    Hang,
    Refused,
}

fn failure_of(script: &str) -> Option<Failure> {
    Some(match script {
        "raises authentication" => Failure::Status(401),
        "raises rate limit" => Failure::Status(429),
        "raises timeout" => Failure::Hang,
        "raises connection" => Failure::Refused,
        "raises permission denied" => Failure::Status(403),
        "raises server" => Failure::Status(500),
        "raises status" => Failure::Status(400),
        _ => return None,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn openai_compatible_asks_and_reads_as_python_did() {
    let golden = golden();
    let cases = golden["openai"].as_array().unwrap();
    assert!(cases.len() > 150);
    let (mut answered, mut failed) = (0, 0);
    for case in cases {
        let script = case["script"].as_str().unwrap();
        let engine = case["engine"].as_str().unwrap();
        let label = format!("{engine}: {script} force={} limits={}/{}", case["force_language"], case["avg_logprob"], case["no_speech_prob"]);

        let (base, server, expected_failure) = if let Some(failure) = failure_of(script) {
            failed += 1;
            match failure {
                Failure::Status(status) => {
                    let server = mock(vec![(status, "{\"error\":{\"message\":\"no\"}}".to_string())]).await;
                    (server.base(), Some(server), true)
                }
                Failure::Hang => (format!("http://127.0.0.1:{}", hang().await), None, true),
                Failure::Refused => (format!("http://127.0.0.1:{}", closed_port()), None, true),
            }
        } else if script == "raises something else" {
            // An error that is no HTTP failure at all has no counterpart here.
            continue;
        } else {
            answered += 1;
            let server = mock(vec![(200, case["reply"].to_string())]).await;
            (server.base(), Some(server), false)
        };

        let provider = OpenAiCompatible::new("KEY-2", &format!("{base}/v1"), case.get("model").and_then(Value::as_str).unwrap_or("whisper-1"), engine)
            .with_wait(if script == "raises connection" { Duration::from_secs(15) } else { Duration::from_millis(400) });
        let (pcm, language, country) = (audio_bytes(if expected_failure { 320 } else { 6400 }, if expected_failure { 1 } else { 5 }), "English", "United States");
        let request = Request {
            pcm: &pcm,
            format: MONO_16K,
            language,
            country,
            avg_logprob: case.get("avg_logprob").and_then(Value::as_f64).unwrap_or(-0.8),
            no_speech_prob: case.get("no_speech_prob").and_then(Value::as_f64).unwrap_or(0.6),
            no_repeat_ngram_size: 0,
            force_language: case.get("force_language").and_then(Value::as_bool).unwrap_or(true),
        };
        let got = provider.recognize(&request).await;
        assert_result(&label, &case["result"], &got);

        if expected_failure {
            continue;
        }
        let server = server.unwrap();
        let sent = &server.requests()[0];
        let expected = &case["request"];
        assert!(sent.request_line.starts_with("POST /v1/audio/transcriptions"), "{label}: {}", sent.request_line);
        assert_eq!(Some("Bearer KEY-2"), sent.header("authorization"), "{label}");
        let upload = parse_multipart(sent.header("content-type").unwrap(), &sent.raw);
        assert_eq!(expected["file_name"].as_str().unwrap(), upload.file_name, "{label}");
        assert_eq!(expected["file_type"].as_str().unwrap(), upload.file_type, "{label}");
        assert_eq!(expected["file_len"].as_u64().unwrap() as usize + 44, upload.file.len(), "{label}");
        assert_eq!(expected["file_sha"].as_str().unwrap(), sha(&upload.file[44..]), "{label}: audio");
        let field = |name: &str| upload.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
        assert_eq!(expected["model"].as_str(), field("model"), "{label}");
        assert_eq!(expected["language"].as_str(), field("language"), "{label}: language field");
        assert_eq!(expected["response_format"].as_str(), field("response_format"), "{label}");
        assert_eq!(Some("0.0"), field("temperature"), "{label}");
        assert_eq!(0.0, expected["temperature"].as_f64().unwrap(), "{label}");
    }
    assert!(answered > 140 && failed >= 7);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_openai_request_is_retried_but_a_wrong_key_is_not() {
    let busy = mock(vec![(429, "{}".to_string()), (200, "{\"text\":\"ok\"}".to_string())]).await;
    let provider = OpenAiCompatible::new("K", &format!("{}/v1", busy.base()), "m", "Custom_Whisper");
    let pcm = audio_bytes(320, 1);
    let request = Request {
        pcm: &pcm,
        format: MONO_16K,
        language: "English",
        country: "United States",
        avg_logprob: -0.8,
        no_speech_prob: 0.6,
        no_repeat_ngram_size: 0,
        force_language: false,
    };
    let got = provider.recognize(&request).await.unwrap();
    assert_eq!("ok", got.text);
    assert_eq!(2, busy.requests().len(), "the rate limit is retried once and the second try is answered");

    let wrong = mock(vec![(401, "{}".to_string())]).await;
    let provider = OpenAiCompatible::new("K", &format!("{}/v1", wrong.base()), "m", "Custom_Whisper");
    assert_eq!(
        Err(RecognizeError::Api { code: "TRANSCRIPTION_API_AUTH_FAILED".into() }),
        provider.recognize(&request).await
    );
    assert_eq!(1, wrong.requests().len());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_language_missing_from_the_table_is_a_key_error_as_in_python() {
    let server = mock(vec![(200, "{\"text\":\"x\"}".to_string())]).await;
    let provider = OpenAiCompatible::new("K", &format!("{}/v1", server.base()), "m", "Custom_Whisper");
    let pcm = audio_bytes(320, 1);
    let mut request = Request {
        pcm: &pcm,
        format: MONO_16K,
        language: "Nowhere",
        country: "Nowhere",
        avg_logprob: -0.8,
        no_speech_prob: 0.6,
        no_repeat_ngram_size: 0,
        force_language: true,
    };
    assert_eq!(Err(RecognizeError::Other { kind: "KeyError".into() }), provider.recognize(&request).await);
    assert!(server.requests().is_empty(), "the lookup fails before anything is sent");
    request.force_language = false;
    assert_eq!("x", provider.recognize(&request).await.unwrap().text);
}

// ---- model lists ----

#[tokio::test(flavor = "multi_thread")]
async fn deepgram_model_lists_match_python() {
    let golden = golden();
    for row in golden["models"]["deepgram"].as_array().unwrap() {
        let base = match row["status"].as_u64() {
            Some(status) => mock(vec![(status as u16, row["body"].to_string())]).await.base(),
            None => format!("http://127.0.0.1:{}", closed_port()),
        };
        let detailed = deepgram::models_detailed(&base, "K").await;
        let expected: Vec<(String, Vec<String>)> = row["detailed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (m["name"].as_str().unwrap().to_string(), strings(&m["languages"])))
            .collect();
        let got: Vec<(String, Vec<String>)> = detailed.iter().map(|m| (m.name.clone(), m.languages.clone())).collect();
        assert_eq!(expected, got, "{row}");
        assert_eq!(strings(&row["names"]), deepgram::models(&base, "K").await, "{row}");
        assert_eq!(row["check"].as_bool().unwrap(), deepgram::check_api_key(&base, "K").await, "{row}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn openai_model_lists_match_python() {
    let golden = golden();
    for row in golden["models"]["openai"].as_array().unwrap() {
        let (base, ids) = match row["ids"].as_array() {
            Some(ids) => {
                let body = serde_json::json!({"data": ids.iter().map(|id| serde_json::json!({"id": id})).collect::<Vec<_>>()});
                (format!("{}/v1", mock(vec![(200, body.to_string())]).await.base()), true)
            }
            None => (format!("http://127.0.0.1:{}/v1", closed_port()), false),
        };
        let keywords: Option<Vec<String>> = row["keywords"].as_array().map(|k| strings(&Value::Array(k.clone())));
        let keyword_refs: Option<Vec<&str>> = keywords.as_ref().map(|k| k.iter().map(String::as_str).collect());
        let got = openai::available_models(&base, "K", keyword_refs.as_deref()).await;
        match (row["models"].as_array(), &got) {
            (Some(expected), Ok(models)) => assert_eq!(strings(&Value::Array(expected.clone())), *models, "{row}"),
            (None, Err(_)) => {}
            _ => panic!("{row}: Rust gave {got:?}"),
        }
        assert_eq!(row["check"].as_bool().unwrap(), openai::check_api_key(&base, "K").await, "{row}");
        assert_eq!(ids, got.is_ok());
    }
}

// ---- as a Recognizer ----

#[test]
fn a_cloud_engine_can_be_called_from_the_transcribers_thread() {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let server = runtime.block_on(mock(vec![(200, "{\"text\":\"from a thread\",\"language\":\"en\"}".to_string())]));
    let provider = OpenAiCompatible::new("K", &format!("{}/v1", server.base()), "m", "OpenAI_Whisper");
    let mut engine = Blocking::new(provider, runtime.handle().clone());
    let pcm = audio_bytes(320, 1);
    let found = engine
        .recognize(&Request {
            pcm: &pcm,
            format: MONO_16K,
            language: "English",
            country: "United States",
            avg_logprob: -0.8,
            no_speech_prob: 0.6,
            no_repeat_ngram_size: 0,
            force_language: true,
        })
        .unwrap();
    assert_eq!(("from a thread", 0.5, true), (found.text.as_str(), found.confidence, found.definitive));
}
