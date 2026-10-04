//! The Google speech endpoint replays the historical `custom_speech_recognition`
//! `recognize_google`/`AudioFile` contract: reply parsing, requests and stereo mixing.
//! `fixtures/google_golden.json` is frozen at `16cb286c`; see `fixtures/README.md`.
//!
//! The FLAC encoder differs from Python's `flac --best`; decoded samples must match.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{closed_port, hang, mock};
use serde_json::Value;
use sha2::{Digest, Sha256};
use vrct_core::transcription::clip::{to_16k_mono, to_mono_16bit, tomono_sum};
use vrct_core::transcription::cloud::CloudRecognizer;
use vrct_core::transcription::google::{flac, parse_reply, GoogleProvider, Heard};
use vrct_core::transcription::phrases::{Format, RecognizeError, Request};

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/google_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn unhex(text: &str) -> Vec<u8> {
    hex::decode(text).unwrap()
}

#[test]
fn replies_are_read_as_recognize_google_reads_them() {
    let golden = golden();
    let cases = golden["replies"].as_array().unwrap();
    assert!(cases.len() >= 100);
    let (mut texts, mut unknown, mut errors) = (0, 0, 0);
    for case in cases {
        let label = format!("{} {:?}", case["label"], case["reply"]);
        let expected = &case["expected"];
        let got = parse_reply(case["reply"].as_str().unwrap());
        if expected.get("unknown").is_some() {
            assert_eq!(Ok(Heard::Nothing), got, "{label}");
            unknown += 1;
        } else if let Some(kind) = expected.get("error") {
            match got {
                Err(RecognizeError::Other { kind: got }) => assert_eq!(kind.as_str().unwrap(), got, "{label}"),
                other => panic!("{label}: Python raised {kind}, Rust gave {other:?}"),
            }
            errors += 1;
        } else {
            let Ok(Heard::Text { text, confidence }) = got else { panic!("{label}: Python heard {expected}, Rust gave {got:?}") };
            assert_eq!(expected["text"].as_str().unwrap(), text, "{label}: text");
            assert_eq!(expected["confidence"].as_f64().unwrap(), confidence, "{label}: confidence");
            texts += 1;
        }
    }
    assert!(texts >= 40 && unknown >= 30 && errors >= 15, "{texts} texts, {unknown} unknown, {errors} errors");
}

#[test]
fn stereo_is_summed_to_mono_as_audioop_does() {
    let golden = golden();
    let cases = golden["tomono"].as_array().unwrap();
    assert_eq!(24, cases.len());
    for case in cases {
        let width = case["width"].as_u64().unwrap() as usize;
        let stereo = unhex(case["stereo"].as_str().unwrap());
        assert_eq!(unhex(case["mono"].as_str().unwrap()), tomono_sum(&stereo, width).unwrap(), "width {width}, {} bytes", stereo.len());
    }
    assert!(tomono_sum(&[0; 3], 2).is_err());
    // Half a frame is not a whole number of frames either.
    assert!(tomono_sum(&[0; 2], 2).is_err());
    assert!(tomono_sum(&[0; 6], 2).is_err());
    assert_eq!(Vec::<u8>::new(), tomono_sum(&[], 2).unwrap());
    assert!(tomono_sum(&[0; 4], 5).is_err());
}

#[test]
fn audio_file_mono_and_the_16k_clip_are_pythons() {
    let golden = golden();
    let cases = golden["mono"].as_array().unwrap();
    assert!(cases.len() >= 30);
    for case in cases {
        let rate = case["rate"].as_u64().unwrap() as u32;
        let stereo = unhex(case["stereo"].as_str().unwrap());
        let label = format!("{rate} Hz {} frames {}", case["frames"], case["mode"]);
        let format = Format { sample_rate: rate, sample_width: 2, channels: 2 };

        // What `AudioFile.record` returns, at the clip's own rate and width.
        assert_eq!(case["mono_sha"].as_str().unwrap(), sha(&tomono_sum(&stereo, 2).unwrap()), "{label}: mono");
        assert_eq!(case["mono_sha"].as_str().unwrap(), sha(&to_mono_16bit(&stereo, format).unwrap()), "{label}: mono 16-bit");
        // What the engines get from it: `get_raw_data(convert_rate=16000, convert_width=2)`.
        assert_eq!(case["pcm16k_sha"].as_str().unwrap(), sha(&to_16k_mono(&stereo, format).unwrap()), "{label}: 16 kHz");
    }
}

/// The samples of a FLAC file, and its sample rate.
fn decode(flac: &[u8]) -> (Vec<i32>, u32, u32) {
    let mut reader = claxon::FlacReader::new(flac).expect("a valid FLAC file");
    let info = reader.streaminfo();
    assert_eq!(1, info.channels);
    assert_eq!(16, info.bits_per_sample);
    let samples = reader.samples().map(|sample| sample.expect("a readable sample")).collect();
    (samples, info.sample_rate, info.bits_per_sample)
}

#[test]
fn the_flac_holds_the_samples_it_was_given() {
    let mut state = 12345u32;
    let mut noise = |count: usize| -> Vec<u8> {
        (0..count)
            .flat_map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                ((state >> 16) as u16 as i16).to_le_bytes()
            })
            .collect()
    };
    let tone: Vec<u8> = (0..48_000).flat_map(|i| (((i as f64 * 0.05).sin() * 12000.0) as i16).to_le_bytes()).collect();
    let extremes: Vec<u8> = (0..9000).flat_map(|i| [i16::MIN, i16::MAX, 0, 1, -1][i % 5].to_le_bytes()).collect();
    let clips: Vec<(&str, Vec<u8>, u32)> = vec![
        ("one sample", noise(1), 16000),
        ("a few", noise(7), 16000),
        ("just enough", noise(16), 16000),
        ("one block", noise(4096), 16000),
        ("one block and a sample", noise(4097), 48000),
        ("one block and fifteen", noise(4096 + 15), 48000),
        ("one block and sixteen", noise(4096 + 16), 48000),
        ("seven blocks", noise(4096 * 7 - 5), 44100),
        ("silence", vec![0; 16000 * 2], 16000),
        ("a tone", tone, 48000),
        ("the extremes", extremes, 8000),
        ("long noise", noise(160_000), 16000),
    ];
    for (label, pcm, rate) in clips {
        let file = flac(&pcm, rate).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(b"fLaC", &file[..4], "{label}");
        let (samples, got_rate, _) = decode(&file);
        let expected: Vec<i32> = pcm.chunks_exact(2).map(|p| i32::from(i16::from_le_bytes([p[0], p[1]]))).collect();
        assert_eq!(rate, got_rate, "{label}: sample rate");
        // A last block under 16 samples is padded with silence, which strict decoders need.
        let padding = samples.len() - expected.len();
        assert!(padding < 16, "{label}: {padding} samples added");
        assert!(expected[..] == samples[..expected.len()], "{label}: samples differ");
        assert!(samples[expected.len()..].iter().all(|s| *s == 0), "{label}: padding is silence");
        let last_block = if samples.len() > 4096 { samples.len() % 4096 } else { samples.len() };
        assert!(last_block == 0 || last_block >= 16, "{label}: the last block has {last_block} samples");
    }
}

#[test]
fn an_empty_clip_is_still_a_flac_file() {
    let file = flac(&[], 16000).unwrap();
    assert_eq!(b"fLaC", &file[..4]);
    assert!(decode(&file).0.is_empty());
}

fn request<'a>(pcm: &'a [u8], format: Format, language: &'a str, country: &'a str) -> Request<'a> {
    Request { pcm, format, language, country, avg_logprob: -0.8, no_speech_prob: 0.6, no_repeat_ngram_size: 0, force_language: true }
}

const MONO: fn(u32) -> Format = |rate| Format { sample_rate: rate, sample_width: 2, channels: 1 };

#[tokio::test(flavor = "multi_thread")]
async fn the_request_is_what_recognize_google_sends() {
    let golden = golden();
    let cases = golden["requests"].as_array().unwrap();
    assert!(cases.len() >= 60);
    let pcm: Vec<u8> = [1u8, 0].repeat(160);
    for case in cases {
        let rate = case["rate"].as_u64().unwrap() as u32;
        let (language, country) = (case["language"].as_str().unwrap(), case["country"].as_str().unwrap());
        let label = format!("{language}/{country} at {rate} Hz");
        let server = mock(vec![(200, "{\"result\":[]}\n".to_string())]).await;
        let provider = GoogleProvider::new().with_endpoint(&format!("{}/speech-api/v2/recognize", server.base()));
        let got = provider.recognize(&request(&pcm, MONO(rate), language, country)).await;

        if rate < 8000 {
            // Python resamples such a clip to 8 kHz; no device offers it, so it is refused here.
            assert_eq!(Err(RecognizeError::Other { kind: "error".into() }), got, "{label}");
            assert!(server.requests().is_empty(), "{label}");
            continue;
        }
        assert_eq!(Ok((String::new(), 0.0, false)), got.map(|r| (r.text, r.confidence, r.definitive)), "{label}");
        let sent = server.requests();
        assert_eq!(1, sent.len(), "{label}");
        let python = &case["request"];
        let python_url = python["url"].as_str().unwrap();
        let python_query = python_url.split_once('?').unwrap().1;
        assert_eq!(format!("POST /speech-api/v2/recognize?{python_query} HTTP/1.1"), sent[0].request_line, "{label}");
        assert_eq!(python["headers"]["Content-type"].as_str(), sent[0].header("content-type"), "{label}: content type");
        // Python asked the encoder for 16-bit audio at the clip's own rate.
        assert_eq!(Some(2), case["flac"]["convert_width"].as_u64(), "{label}");
        assert_eq!(rate < 8000, !case["flac"]["convert_rate"].is_null(), "{label}");
        let (samples, got_rate, _) = decode(&sent[0].raw);
        assert_eq!(rate, got_rate, "{label}: FLAC rate");
        assert_eq!(160, samples.len(), "{label}: samples");
        assert!(samples.iter().all(|s| *s == 1), "{label}: sample values");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stereo_clip_is_posted_as_the_sum_of_its_channels() {
    let server = mock(vec![(200, "{\"result\":[]}\n".to_string())]).await;
    let provider = GoogleProvider::new().with_endpoint(&server.base());
    let mut frames: Vec<[i16; 2]> = vec![[1000, 3000], [-32768, -1], [30000, 30000], [5, -9]];
    frames.extend([[7, 8]; 40]);
    let pcm: Vec<u8> = frames.iter().flatten().flat_map(|s| s.to_le_bytes()).collect();
    let format = Format { sample_rate: 48000, sample_width: 2, channels: 2 };
    provider.recognize(&request(&pcm, format, "Japanese", "Japan")).await.unwrap();
    let (samples, rate, _) = decode(&server.requests()[0].raw);
    assert_eq!(48000, rate);
    assert_eq!(vec![4000, -32768, 32767, -4], samples[..4]);
    assert!(samples[4..].iter().all(|s| *s == 15));
}

async fn answer(status: u16, body: &str) -> Result<(String, f64, bool), RecognizeError> {
    let server = mock(vec![(status, body.to_string())]).await;
    let provider = GoogleProvider::new().with_endpoint(&server.base());
    let pcm = [1u8, 0].repeat(160);
    provider.recognize(&request(&pcm, MONO(16000), "Japanese", "Japan")).await.map(|r| (r.text, r.confidence, r.definitive))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_becomes_a_recognition() {
    let two = "{\"result\":[]}\n{\"result\":[{\"alternative\":[{\"transcript\":\"hello\",\"confidence\":0.5}]}]}\n{\"result\":[{\"alternative\":[{\"transcript\":\"world\",\"confidence\":0.75}]}]}\n";
    assert_eq!(Ok(("hello world".to_string(), 0.625, false)), answer(200, two).await);
    assert_eq!(Ok((String::new(), 0.0, false)), answer(200, "{\"result\":[]}\n").await);
    assert_eq!(Ok((String::new(), 0.0, false)), answer(200, "").await);
    assert_eq!(Ok((String::new(), 0.0, false)), answer(204, "").await);
    assert_eq!(Err(RecognizeError::Other { kind: "JSONDecodeError".into() }), answer(200, "oops\n").await);
    assert_eq!(Err(RecognizeError::Other { kind: "KeyError".into() }), answer(200, "{}\n").await);
    // urlopen raises on 4xx and 5xx; the library reports them as RequestError.
    for status in [400, 403, 429, 500, 503] {
        assert_eq!(Err(RecognizeError::Other { kind: "RequestError".into() }), answer(status, two).await, "status {status}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_connection_or_a_wait_that_runs_out_is_a_request_error() {
    let pcm = [1u8, 0].repeat(160);
    let refused = GoogleProvider::new().with_endpoint(&format!("http://127.0.0.1:{}", closed_port())).with_wait(Duration::from_secs(15));
    assert_eq!(Err(RecognizeError::Other { kind: "RequestError".into() }), refused.recognize(&request(&pcm, MONO(16000), "Japanese", "Japan")).await.map(|_| ()));
    let silent = GoogleProvider::new().with_endpoint(&format!("http://127.0.0.1:{}", hang().await)).with_wait(Duration::from_millis(300));
    assert_eq!(Err(RecognizeError::Other { kind: "RequestError".into() }), silent.recognize(&request(&pcm, MONO(16000), "Japanese", "Japan")).await.map(|_| ()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_language_the_table_lacks_is_a_key_error() {
    let provider = GoogleProvider::new().with_endpoint("http://127.0.0.1:9");
    let pcm = [1u8, 0].repeat(160);
    assert_eq!(Err(RecognizeError::Other { kind: "KeyError".into() }), provider.recognize(&request(&pcm, MONO(16000), "Klingon", "Qo'noS")).await.map(|_| ()));
}
