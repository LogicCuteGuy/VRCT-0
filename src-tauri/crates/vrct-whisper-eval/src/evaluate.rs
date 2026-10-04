use crate::{audio, dataset::resolve_dataset_path, invalid, metrics::character_error_rate, Result};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::Instant,
};
use vrct_core::{
    audio::{
        silero::{OnnxRuntime, SileroFrameProbability},
        vad::{SpeechSegment, VadConfig, VadSegmenter},
        FRAME_BYTES,
    },
    transcription::{
        cloud::Blocking,
        google::GoogleProvider,
        native::{FileWhisper, WhisperLoader},
        phrases::{Chunk, Engine, Format, PhraseTranscriber, Query, Recognizer, Settings, Stamp},
    },
};
#[derive(Clone, Debug)]
pub struct Evaluate {
    pub repo_root: PathBuf,
    pub dataset_dir: PathBuf,
    pub output: PathBuf,
    pub condition: String,
    pub item_id: Option<String>,
    pub engine: String,
    pub model: String,
    pub compute_type: String,
    pub ort_library: Option<PathBuf>,
    pub all: bool,
}
pub fn load_rows(
    root: &Path,
    condition: &str,
    id: Option<&str>,
    all: bool,
) -> Result<Vec<csv::StringRecord>> {
    let mut reader = csv::Reader::from_path(root.join("metadata.csv"))?;
    let headers = reader.headers()?.clone();
    for name in [
        "id",
        "condition",
        "audio_path",
        "transcript",
        "duration_seconds",
    ] {
        if !headers.iter().any(|h| h == name) {
            return Err(invalid(format!("metadata.csv missing column: {name}")));
        }
    }
    let mut rows = Vec::new();
    for row in reader.records() {
        let row = row?;
        let value = |name| {
            row.get(headers.iter().position(|h| h == name).unwrap())
                .unwrap_or("")
        };
        if (condition == "all" || value("condition") == condition)
            && id.is_none_or(|id| value("id") == id)
        {
            rows.push(row);
            if !all {
                break;
            }
        }
    }
    if rows.is_empty() {
        return Err(invalid("metadata.csv has no matching condition/id"));
    }
    // Keep field names with each record as JSON via the separate loader below.
    Ok(rows)
}
pub fn metadata_rows(config: &Evaluate) -> Result<Vec<Value>> {
    let records = load_rows(
        &config.dataset_dir,
        &config.condition,
        config.item_id.as_deref(),
        config.all,
    )?;
    let mut reader = csv::Reader::from_path(config.dataset_dir.join("metadata.csv"))?;
    let headers = reader.headers()?.clone();
    Ok(records
        .into_iter()
        .map(|r| {
            Value::Object(
                headers
                    .iter()
                    .zip(r.iter())
                    .map(|(k, v)| (k.into(), json!(v)))
                    .collect(),
            )
        })
        .collect())
}
fn pcm_bytes(path: &Path) -> Result<Vec<u8>> {
    let pcm = audio::read_pcm16_wav(path)?;
    if !pcm.standard() {
        return Err(invalid("evaluation input must be16k mono PCM16 WAV"));
    }
    Ok(pcm
        .samples
        .iter()
        .flat_map(|x| ((*x * 32768.).round_ties_even().clamp(-32768., 32767.) as i16).to_le_bytes())
        .collect())
}

/// Production phrase accumulation, padding, recognition counters and transcript
/// storage. A test recognizer can be injected here, never through a CLI option.
pub fn evaluate_segments(
    segments: Vec<SpeechSegment>,
    engine: Engine,
    recognizer: &mut dyn Recognizer,
) -> Value {
    let mut transcriber = PhraseTranscriber::new(Settings {
        speaker: false,
        format: Format {
            sample_rate: 16000,
            sample_width: 2,
            channels: 1,
        },
        phrase_timeout: 3,
        max_phrases: 10,
        engine,
        segmented: true,
    });
    let languages = vec!["Japanese".into()];
    let countries = vec!["Japan".into()];
    let query = Query {
        languages: &languages,
        countries: &countries,
        avg_logprob: -0.8,
        no_speech_prob: 0.6,
        no_repeat_ngram_size: 0,
    };
    let reasons: Vec<_> = segments.iter().map(|s| s.reason.as_str()).collect();
    let count = segments.len();
    let mut timestamp = 0;
    let mut queue: VecDeque<Chunk> = segments
        .into_iter()
        .map(|s| {
            let at = Stamp(timestamp);
            timestamp += (s.duration_ms() * 1000.) as i64;
            Chunk {
                data: s.audio,
                at,
                end: Some(s.reason),
            }
        })
        .collect();
    let mut hypotheses = Vec::new();
    let mut confidence = Vec::new();
    let mut transcribed = false;
    let mut pipeline_error = String::new();
    let started = Instant::now();
    while !queue.is_empty() {
        let before = queue.len();
        match transcriber.transcribe_queue(
            &mut queue,
            Some(recognizer),
            &query,
            Stamp(timestamp + 4_000_000),
        ) {
            Ok(sent) => transcribed |= sent,
            Err(error) => {
                pipeline_error = error.exception_type;
                break;
            }
        }
        while transcriber.has_transcript() {
            let transcript = transcriber.take_transcript();
            hypotheses.push(transcript.text);
            confidence.push(transcript.confidence);
        }
        if queue.len() == before {
            pipeline_error = "PipelineQueueDidNotDrain".into();
            break;
        }
    }
    // VAD flush normally ensures a natural final boundary. Reject unexpected
    // unsent audio rather than reporting a successful partial evaluation.
    if transcriber.buffered_len() != 0 && pipeline_error.is_empty() {
        pipeline_error = "UnfinalizedVadSegment".into();
    }
    let hypothesis = hypotheses.join("");
    json!({"hypothesis":hypothesis,"vad_segment_count":count,"vad_reasons":reasons,"queue_drained":queue.is_empty(),"transcribed":transcribed,"asr_attempts":transcriber.asr_attempts(),"asr_successes":transcriber.asr_successes(),"pipeline_error":pipeline_error,"last_recognition_error":transcriber.last_recognition_error(),"confidence":if confidence.is_empty(){0.}else{confidence.iter().sum::<f64>()/confidence.len() as f64},"asr_seconds":started.elapsed().as_secs_f64(),"passed":!hypothesis.is_empty()&&queue.is_empty()&&transcriber.asr_successes()>0&&pipeline_error.is_empty()})
}
pub struct Evaluator {
    config: Evaluate,
    segmenter: VadSegmenter<SileroFrameProbability>,
    recognizer: Box<dyn Recognizer + Send>,
    runtime: tokio::runtime::Runtime,
    vad_init_seconds: f64,
    model_load_seconds: f64,
}
impl Evaluator {
    pub fn new(config: Evaluate) -> Result<Self> {
        if !["Whisper", "Google"].contains(&config.engine.as_str()) {
            return Err(invalid("engine must be Whisper or Google"));
        }
        let runtime = tokio::runtime::Runtime::new()?;
        // A missing local model is fatal, never an implicit Google fallback.
        let model_dir = config.repo_root.join("weights/whisper").join(&config.model);
        if config.engine == "Whisper" && !FileWhisper.available(&model_dir) {
            return Err(invalid(format!(
                "Whisper model missing at {}; download it through VRCT first",
                model_dir.display()
            )));
        }
        let vad_started = Instant::now();
        let library = config
            .ort_library
            .clone()
            .or_else(OnnxRuntime::locate)
            .or_else(|| {
                let path = config
                    .repo_root
                    .join("src-tauri/resources/onnxruntime/onnxruntime.dll");
                path.is_file().then_some(path)
            })
            .ok_or_else(|| invalid("ONNX Runtime missing; use --ort-library or ORT_DYLIB_PATH"))?;
        let segmenter = VadSegmenter::new(
            SileroFrameProbability::with_library(&library).map_err(invalid)?,
            VadConfig::default(),
        );
        let vad_init_seconds = vad_started.elapsed().as_secs_f64();
        let started = Instant::now();
        let recognizer: Box<dyn Recognizer + Send> = if config.engine == "Whisper" {
            FileWhisper
                .load(&model_dir, "cpu", 0, &config.compute_type)
                .map_err(invalid)?
        } else {
            Box::new(Blocking::new(
                GoogleProvider::new(),
                runtime.handle().clone(),
            ))
        };
        let model_load_seconds = started.elapsed().as_secs_f64();
        Ok(Self {
            config,
            segmenter,
            recognizer,
            runtime,
            vad_init_seconds,
            model_load_seconds,
        })
    }
    pub fn run(&mut self, row: &Value) -> Result<Value> {
        let _runtime = &self.runtime; // own the executor for the blocking Google provider.
        let text = |name| row[name].as_str().unwrap_or("");
        let path = resolve_dataset_path(&self.config.dataset_dir, text("audio_path"))?;
        let raw = pcm_bytes(&path)?;
        let duration = raw.len() as f64 / 32000.;
        if duration == 0. {
            return Err(invalid("evaluation clip is empty"));
        }
        self.segmenter.reset();
        let started = Instant::now();
        let mut segments = Vec::new();
        for chunk in raw.chunks(FRAME_BYTES * 4) {
            segments.extend(self.segmenter.process(chunk).map_err(invalid)?);
        }
        if let Some(segment) = self.segmenter.flush().map_err(invalid)? {
            segments.push(segment);
        }
        let vad_seconds =
            started.elapsed().as_secs_f64() + std::mem::take(&mut self.vad_init_seconds);
        if segments.is_empty() {
            return Err(invalid(format!(
                "VAD emitted no speech segment for {}",
                path.display()
            )));
        }
        let mut result = evaluate_segments(
            segments,
            if self.config.engine == "Whisper" {
                Engine::Whisper
            } else {
                Engine::Google
            },
            self.recognizer.as_mut(),
        );
        let hypothesis = result["hypothesis"].as_str().unwrap_or("").to_owned();
        let asr = result["asr_seconds"].as_f64().unwrap_or(0.);
        let manifest = self.config.dataset_dir.join("manifest.json");
        let fields = json!({"test":"vrct_vad_to_transcription","engine":self.config.engine,"model":if self.config.engine=="Whisper"{Some(&self.config.model)}else{None},"compute_type":if self.config.engine=="Whisper"{Some(&self.config.compute_type)}else{None},"condition":text("condition"),"id":text("id"),"audio_path":text("audio_path"),"reference":text("transcript"),"cer":character_error_rate(text("transcript"),&hypothesis),"vad_seconds":vad_seconds,"model_load_seconds":std::mem::take(&mut self.model_load_seconds),"audio_duration_seconds":duration,"real_time_factor":asr/duration,"pipeline_real_time_factor":(vad_seconds+asr)/duration,"dataset_manifest_sha256":if manifest.is_file(){Some(audio::sha256_file(&manifest)?)}else{None}});
        result
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        Ok(result)
    }
}
pub fn run(config: Evaluate) -> Result<Value> {
    let rows = metadata_rows(&config)?;
    // Validate all CSV paths before allocating models or making any request.
    let mut inputs = Vec::new();
    for row in &rows {
        inputs.push(resolve_dataset_path(
            &config.dataset_dir,
            row["audio_path"].as_str().unwrap_or(""),
        )?);
    }
    for name in ["metadata.csv", "manifest.json"] {
        let path = config.dataset_dir.join(name);
        if path.exists() {
            inputs.push(path.canonicalize()?);
        }
    }
    crate::dataset::InputProtection::new(inputs)?.check(&config.output)?;
    let output = config.output.clone();
    let all = config.all;
    let mut evaluator = Evaluator::new(config)?;
    let mut results = Vec::new();
    for row in rows {
        match evaluator.run(&row) {
            Ok(result) => results.push(result),
            Err(error) if all => results.push(json!({"test":"vrct_vad_to_transcription","engine":evaluator.config.engine,
                "condition":row["condition"],"id":row["id"],"audio_path":row["audio_path"],"reference":row["transcript"],
                "hypothesis":"","cer":character_error_rate(row["transcript"].as_str().unwrap_or(""),""),"passed":false,
                "pipeline_error":error.to_string(),"asr_attempts":0,"asr_successes":0,"queue_drained":false,
                "vad_seconds":null,"asr_seconds":null,"real_time_factor":null})),
            Err(error) => return Err(error),
        }
    }
    let result = if all {
        json!({"test":"vrct_vad_to_transcription_batch","count":results.len(),"passed":results.iter().all(|r|r["passed"]==true),"results":results})
    } else {
        results.remove(0)
    };
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        output,
        format!("{}\n", serde_json::to_string_pretty(&result)?),
    )?;
    Ok(result)
}
