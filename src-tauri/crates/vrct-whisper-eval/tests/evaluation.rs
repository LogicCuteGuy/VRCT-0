use serde_json::{json, Value};
use std::{path::Path, process::Command};
use vrct_core::{
    audio::{
        silero::SileroFrameProbability,
        vad::{SegmentEnd, SpeechSegment, VadConfig, VadSegmenter},
    },
    transcription::phrases::{Engine, Recognition, RecognizeError, Recognizer, Request},
};
use vrct_whisper_eval::{
    audio::{self, SAMPLE_RATE},
    dataset::{self, Prepare, Row},
    evaluate::{self, evaluate_segments},
    metrics::{character_error_rate, normalize_transcript},
};

#[test]
fn unicode_normalization_and_character_edit_distance_match_python() {
    assert_eq!(normalize_transcript(" ＡＢＣ\n\u{1c}ｶﾞ\u{a0}😀"), "ABCガ😀");
    assert_eq!(character_error_rate("こんにちは", "こんにちは"), 0.);
    assert!((character_error_rate("あいうえお", "あいうお") - 0.2).abs() < 1e-12);
    assert_eq!(character_error_rate("", ""), 0.);
    assert_eq!(character_error_rate("", "😀"), 1.);
    assert_eq!(character_error_rate("😀", "😀😀"), 1.);
    assert_eq!(character_error_rate("日", "日日日"), 2.);
}
fn golden_rows() -> (Value, Vec<Row>) {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/selection.json")).unwrap();
    let rows = fixture["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| Row {
            audio_path: r["audio_path"].as_str().unwrap().into(),
            transcript: r["transcript"].as_str().unwrap().into(),
            speaker_id: r["speaker_id"].as_str().unwrap().into(),
            row_number: r["row_number"].as_u64().unwrap() as usize,
        })
        .collect();
    (fixture, rows)
}
#[test]
fn seeded_selection_and_seed_hash_equal_actual_python_oracle() {
    let (fixture, rows) = golden_rows();
    let selected = dataset::select_rows(&rows, 9, 20260911, 3).unwrap();
    assert_eq!(
        json!(selected.iter().map(|r| &r.audio_path).collect::<Vec<_>>()),
        fixture["selected"]
    );
    assert_eq!(
        audio::deterministic_seed(&["clip", "white_snr20"]),
        fixture["hash_seed"].as_u64().unwrap()
    );
    assert!(dataset::select_rows(&rows, 0, 1, 3).is_err());
    assert!(dataset::select_rows(&rows, 99, 1, 3).is_err());
    assert!(dataset::select_rows(&rows[..6], 3, 1, 3).is_err());
    let mut one_speaker = rows;
    for row in &mut one_speaker {
        row.speaker_id = "only-one".into();
    }
    assert!(dataset::select_rows(&one_speaker, 9, 1, 3).is_err());
}
#[test]
fn tsv_duplicates_unicode_empty_rows_and_speaker_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let tsv = temp.path().join("rows.tsv");
    std::fs::write(&tsv,"path\tsentence\tclient_id\tspeaker_id\nclip\\a.wav\t ＡＢＣ \t\tother\nclip/a.wav\tduplicate\ta\tx\nempty.wav\t \tb\ty\nnext.wav\t日本語\t  \treal\n").unwrap();
    let rows = dataset::load_rows(&tsv).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].audio_path, "clip/a.wav");
    assert_eq!(rows[0].speaker_id, "other");
    assert_eq!(rows[0].transcript, "ＡＢＣ");
    assert_eq!(rows[1].speaker_id, "unknown-speaker");
}
#[test]
fn noise_is_reproducible_obeys_snr_and_handles_empty_silence() {
    let clean = vec![0.1f32; 16000];
    let seed = audio::deterministic_seed(&["clip", "white_snr20"]);
    let a = audio::make_noise_variant(&clean, "white", 20., seed, None).unwrap();
    let b = audio::make_noise_variant(&clean, "white", 20., seed, None).unwrap();
    assert_eq!(a, b);
    assert_ne!(a, clean);
    let noise_rms =
        (a.iter().map(|x| (*x as f64 - 0.1).powi(2)).sum::<f64>() / a.len() as f64).sqrt();
    assert!((20. * (0.1 / noise_rms).log10() - 20.).abs() < 0.001);
    assert_eq!(
        audio::make_noise_variant(&[0.; 32], "white", 10., seed, None).unwrap(),
        vec![0.; 32]
    );
    assert!(audio::make_noise_variant(&clean, "environment", 10., seed, Some(&[])).is_err());
    assert!(audio::make_noise_variant(&clean, "invalid", 10., seed, None).is_err());
    assert!(audio::mix_at_snr(&clean, &[f32::NAN; 16000], 10.).is_err());
    let env = [-0.1, 0.3, 0.2, -0.4];
    assert_eq!(
        audio::make_noise_variant(&clean, "environment", 10., seed, Some(&env)).unwrap(),
        audio::make_noise_variant(&clean, "environment", 10., seed, Some(&env)).unwrap()
    );
}
#[test]
fn wav_roundtrip_strict_format_and_conversion_needs_no_ffmpeg_for_standard() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a.wav");
    let b = temp.path().join("b.wav");
    audio::write_pcm16_wav(&a, &[-1., 0., 1., 0.1], SAMPLE_RATE).unwrap();
    let pcm = audio::read_pcm16_wav(&a).unwrap();
    assert!(pcm.standard());
    assert_eq!(pcm.samples.len(), 4);
    audio::convert_to_standard_wav(&a, &b, "executable-that-does-not-exist").unwrap();
    assert!(audio::read_pcm16_wav(&b).unwrap().standard());
    assert!(audio::convert_to_standard_wav(&a, &a, "ffmpeg").is_err());
    let link = temp.path().join("hardlink.wav");
    std::fs::hard_link(&a, &link).unwrap();
    let before = std::fs::read(&a).unwrap();
    assert!(audio::convert_to_standard_wav(&a, &link, "ffmpeg").is_err());
    assert_eq!(std::fs::read(&a).unwrap(), before);
    let mut bytes = std::fs::read(&a).unwrap();
    bytes.pop();
    std::fs::write(&b, bytes).unwrap();
    assert!(audio::read_pcm16_wav(&b).is_err());
    std::fs::write(&b, b"RIFF").unwrap();
    assert!(audio::read_pcm16_wav(&b).is_err());
    assert!(audio::write_pcm16_wav(&b, &[0.], u32::MAX).is_err());
    assert_eq!(std::fs::read(&b).unwrap(), b"RIFF");
}
#[test]
fn extensible_pcm16_and_actual_ffmpeg_resampling_are_supported() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.wav");
    let destination = temp.path().join("converted.wav");
    audio::write_pcm16_wav(&source, &vec![0.1; 4800], 48000).unwrap();
    let mut bytes = std::fs::read(&source).unwrap();
    // Upgrade the existing PCM fmt chunk to the Windows extensible PCM GUID.
    bytes[16..20].copy_from_slice(&40u32.to_le_bytes());
    bytes[20..22].copy_from_slice(&0xfffeu16.to_le_bytes());
    let extension: [u8; 24] = [
        22, 0, 16, 0, 4, 0, 0, 0, 1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113,
    ];
    bytes.splice(36..36, extension);
    let size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&size.to_le_bytes());
    std::fs::write(&source, bytes).unwrap();
    let pcm = audio::read_pcm16_wav(&source).unwrap();
    assert_eq!(pcm.sample_rate, 48000);
    assert_eq!(pcm.samples.len(), 4800);
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("Resampling test skipped: ffmpeg not installed");
        return;
    }
    let converted =
        audio::convert_to_standard_wav(&source.canonicalize().unwrap(), &destination, "ffmpeg")
            .unwrap();
    assert_eq!(converted.len(), 1600);
    assert!(audio::read_pcm16_wav(&destination).unwrap().standard());
    assert!(converted.iter().all(|x| x.is_finite()));
}
#[test]
fn source_and_dataset_paths_cannot_escape_and_clip_directory_resolves() {
    let temp = tempfile::tempdir().unwrap();
    let corpus = temp.path().join("corpus");
    std::fs::create_dir_all(corpus.join("clips")).unwrap();
    std::fs::write(corpus.join("clips/clip.wav"), b"test").unwrap();
    std::fs::write(temp.path().join("outside.wav"), b"test").unwrap();
    assert_eq!(
        dataset::resolve_source(&corpus, "clip.wav").unwrap(),
        corpus.join("clips/clip.wav").canonicalize().unwrap()
    );
    assert!(dataset::resolve_source(&corpus, "../outside.wav").is_err());
    assert!(dataset::resolve_source(&corpus, "../missing.wav").is_err());
    assert!(dataset::resolve_dataset_path(&corpus, "../outside.wav").is_err());
    assert!(dataset::resolve_dataset_path(
        &corpus,
        temp.path().join("outside.wav").to_str().unwrap()
    )
    .is_err());
}
fn fixture(root: &Path) -> Prepare {
    let (_, rows) = golden_rows();
    let input = root.join("commonvoice");
    std::fs::create_dir_all(input.join("clips")).unwrap();
    let tsv = input.join("validated.tsv");
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b'\t')
        .from_path(&tsv)
        .unwrap();
    writer
        .write_record(["path", "sentence", "client_id"])
        .unwrap();
    for row in rows {
        audio::write_pcm16_wav(
            &input.join("clips").join(&row.audio_path),
            &vec![0.; 1600],
            SAMPLE_RATE,
        )
        .unwrap();
        writer
            .write_record([row.audio_path, row.transcript, row.speaker_id])
            .unwrap();
    }
    writer.flush().unwrap();
    let environment = root.join("noise.wav");
    audio::write_pcm16_wav(
        &environment,
        &(0..16000)
            .map(|i| (i as f32 * 0.1).sin() * 0.1)
            .collect::<Vec<_>>(),
        SAMPLE_RATE,
    )
    .unwrap();
    Prepare {
        input_dir: input,
        tsv,
        output_dir: root.join("dataset"),
        environment_noise: environment,
        count: 9,
        seed: 20260911,
        ffmpeg: "not-installed".into(),
        source_name: "test dataset".into(),
        source_url: "https://example.invalid/dataset".into(),
        license: "CC0-1.0".into(),
        dataset_version: "test-version".into(),
    }
}
#[test]
fn preparation_writes_all_five_conditions_metadata_manifest_and_hashes() {
    let temp = tempfile::tempdir().unwrap();
    let config = fixture(temp.path());
    let manifest = dataset::prepare_dataset(&config).unwrap();
    assert_eq!(manifest["count"], 9);
    assert_eq!(manifest["source"]["license"], "CC0-1.0");
    assert_eq!(manifest["noise_algorithm"], audio::NOISE_ALGORITHM);
    assert!((manifest["total_clean_duration_seconds"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    let mut reader = csv::Reader::from_path(config.output_dir.join("metadata.csv")).unwrap();
    let headers = reader.headers().unwrap().clone();
    let rows: Vec<_> = reader
        .deserialize::<dataset::Metadata>()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(headers.len(), 15);
    assert_eq!(rows.len(), 45);
    for row in rows {
        assert_eq!(
            audio::read_pcm16_wav(&config.output_dir.join(&row.audio_path))
                .unwrap()
                .samples
                .len(),
            1600
        );
        assert_eq!(row.source_sha256.len(), 64);
    }
    let saved: Value =
        serde_json::from_slice(&std::fs::read(config.output_dir.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(saved, manifest);
}
#[test]
fn preparation_preflight_preserves_later_source_environment_and_tsv() {
    for input in ["source", "hardlink", "environment", "tsv"] {
        let temp = tempfile::tempdir().unwrap();
        let mut config = fixture(temp.path());
        config.output_dir = config.input_dir.clone();
        config.count = 18;
        let protected = match input {
            "source" => {
                let target = config.input_dir.join("white_snr10/cv_0001.wav");
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::copy(config.input_dir.join("clips/clip-03.wav"), &target).unwrap();
                let text = std::fs::read_to_string(&config.tsv).unwrap();
                std::fs::write(
                    &config.tsv,
                    text.replace("clip-03.wav", "white_snr10/cv_0001.wav"),
                )
                .unwrap();
                target
            }
            "environment" => {
                let target = config.input_dir.join("white_snr10/cv_0001.wav");
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::copy(&config.environment_noise, &target).unwrap();
                config.environment_noise = target.clone();
                target
            }
            "hardlink" => {
                let target = config.input_dir.join("white_snr10/cv_0001.wav");
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::hard_link(config.input_dir.join("clips/clip-03.wav"), &target).unwrap();
                target
            }
            _ => {
                let target = config.input_dir.join("metadata.csv");
                std::fs::copy(&config.tsv, &target).unwrap();
                config.tsv = target.clone();
                target
            }
        };
        let before = std::fs::read(&protected).unwrap();
        let error = dataset::prepare_dataset(&config).unwrap_err().to_string();
        assert!(error.contains("overwrite an input"), "{error}");
        assert_eq!(std::fs::read(&protected).unwrap(), before);
        assert!(!config.output_dir.join("clean/cv_0001.wav").exists());
    }
}
#[test]
fn evaluation_cannot_overwrite_dataset_inputs_before_loading_models() {
    let temp = tempfile::tempdir().unwrap();
    let prepared = fixture(temp.path());
    dataset::prepare_dataset(&prepared).unwrap();
    std::fs::hard_link(
        prepared.output_dir.join("clean/cv_0001.wav"),
        prepared.output_dir.join("alias.wav"),
    )
    .unwrap();
    for output in [
        "metadata.csv",
        "manifest.json",
        "clean/cv_0001.wav",
        "alias.wav",
    ] {
        let output = prepared.output_dir.join(output);
        let before = std::fs::read(&output).unwrap();
        let config = evaluate::Evaluate {
            repo_root: temp.path().into(),
            dataset_dir: prepared.output_dir.clone(),
            output: output.clone(),
            condition: "clean".into(),
            item_id: None,
            engine: "Whisper".into(),
            model: "base".into(),
            compute_type: "int8".into(),
            ort_library: None,
            all: false,
        };
        let error = evaluate::run(config).unwrap_err().to_string();
        assert!(error.contains("overwrite an input"), "{error}");
        assert_eq!(std::fs::read(output).unwrap(), before);
    }
}
struct Mock {
    mode: u8,
    calls: usize,
}
impl Recognizer for Mock {
    fn recognize(
        &mut self,
        request: &Request<'_>,
    ) -> std::result::Result<Recognition, RecognizeError> {
        self.calls += 1;
        assert_eq!(request.language, "Japanese");
        assert_eq!(request.country, "Japan");
        assert_eq!(request.pcm.len(), 3200 + 9600 + 16000);
        match self.mode {
            0 => Ok(Recognition {
                text: "これはVRCTのテストです".into(),
                confidence: 0.9,
                definitive: true,
            }),
            1 => Err(RecognizeError::NoMatch),
            _ => Err(RecognizeError::Other {
                kind: "BackendError".into(),
            }),
        }
    }
}
fn segment(reason: SegmentEnd) -> SpeechSegment {
    SpeechSegment {
        audio: vec![1; 3200],
        segment_id: 1,
        reason,
    }
}
#[test]
fn native_phrase_queue_drains_vad_segments_and_stores_whisper_and_google_results() {
    for engine in [Engine::Whisper, Engine::Google] {
        let mut recognizer = Mock { mode: 0, calls: 0 };
        let result = evaluate_segments(
            vec![segment(SegmentEnd::Silence), segment(SegmentEnd::Flush)],
            engine,
            &mut recognizer,
        );
        assert_eq!(
            result["hypothesis"],
            "これはVRCTのテストですこれはVRCTのテストです"
        );
        assert_eq!(result["asr_attempts"], 2);
        assert_eq!(result["asr_successes"], 2);
        assert_eq!(result["queue_drained"], true);
        assert_eq!(result["passed"], true);
    }
}
#[test]
fn no_match_and_asr_failures_preserve_counter_and_storage_contract() {
    let mut recognizer = Mock { mode: 1, calls: 0 };
    let no_match = evaluate_segments(
        vec![segment(SegmentEnd::Flush)],
        Engine::Whisper,
        &mut recognizer,
    );
    assert_eq!(no_match["transcribed"], true);
    assert_eq!(no_match["queue_drained"], true);
    assert_eq!(no_match["asr_attempts"], 1);
    assert_eq!(no_match["asr_successes"], 0);
    assert_eq!(no_match["passed"], false);
    recognizer.mode = 2;
    let failure = evaluate_segments(
        vec![segment(SegmentEnd::Flush)],
        Engine::Whisper,
        &mut recognizer,
    );
    assert_eq!(failure["pipeline_error"], "BackendError");
    assert_eq!(failure["last_recognition_error"], true);
    assert_eq!(failure["passed"], false);
}
#[test]
fn evaluator_missing_local_model_errors_without_cloud_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let config = evaluate::Evaluate {
        repo_root: temp.path().into(),
        dataset_dir: temp.path().into(),
        output: temp.path().join("out.json"),
        condition: "clean".into(),
        item_id: None,
        engine: "Whisper".into(),
        model: "base".into(),
        compute_type: "int8".into(),
        ort_library: None,
        all: false,
    };
    let error = evaluate::Evaluator::new(config).err().unwrap().to_string();
    assert!(error.contains("Whisper model missing"));
}
#[test]
fn real_native_silero_rejects_synthetic_silence_without_any_asr_request() {
    let lib = std::env::var_os("ORT_DYLIB_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../resources/onnxruntime/onnxruntime.dll")
        });
    if !lib.is_file() {
        eprintln!("Silero real-model test skipped: ONNX Runtime not prepared");
        return;
    }
    let mut vad = VadSegmenter::new(
        SileroFrameProbability::with_library(&lib).unwrap(),
        VadConfig::default(),
    );
    assert!(vad.process(&vec![0; 32000]).unwrap().is_empty());
    assert!(vad.flush().unwrap().is_none());
}
#[test]
fn both_native_clis_preserve_options_and_reject_invalid_counts() {
    for bin in [
        env!("CARGO_BIN_EXE_vrct-whisper-prepare"),
        env!("CARGO_BIN_EXE_vrct-transcription-eval"),
    ] {
        assert!(Command::new(bin)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success());
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vrct-whisper-prepare"))
        .args([
            "--input-dir",
            "missing",
            "--output-dir",
            "missing",
            "--environment-noise",
            "missing",
            "--count",
            "6",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--count"));
}
