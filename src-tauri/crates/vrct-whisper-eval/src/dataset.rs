use crate::{audio, invalid, metrics::normalize_transcript, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};
pub const CONDITIONS: [(&str, i32, &str); 4] = [
    ("white", 10, "white_snr10"),
    ("white", 20, "white_snr20"),
    ("environment", 10, "environment_snr10"),
    ("environment", 20, "environment_snr20"),
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub audio_path: String,
    pub transcript: String,
    pub speaker_id: String,
    pub row_number: usize,
}
impl Row {
    pub fn length_bucket(&self) -> &'static str {
        match normalize_transcript(&self.transcript).chars().count() {
            0..=12 => "short",
            40.. => "long",
            _ => "medium",
        }
    }
}
fn trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}
pub fn load_rows(path: &Path) -> Result<Vec<Row>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .flexible(true)
        .from_path(path)?;
    let headers = reader.headers()?.clone();
    let column = |name| headers.iter().position(|h| h == name);
    let p = column("path").ok_or_else(|| invalid("Common Voice TSV missing path column"))?;
    let t =
        column("sentence").ok_or_else(|| invalid("Common Voice TSV missing sentence column"))?;
    let (client, speaker) = (column("client_id"), column("speaker_id"));
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for (i, record) in reader.records().enumerate() {
        let record = record?;
        if record.iter().any(|f| f.len() > 10_000_000) {
            return Err(invalid("TSV field exceeds10MB"));
        }
        let path = trim(record.get(p).unwrap_or("")).replace('\\', "/");
        let text = trim(record.get(t).unwrap_or(""));
        if path.is_empty() || normalize_transcript(text).is_empty() || !seen.insert(path.clone()) {
            continue;
        }
        // Python chooses client_id before stripping, then speaker_id if empty.
        let id = client
            .and_then(|c| record.get(c))
            .filter(|s| !s.is_empty())
            .or_else(|| speaker.and_then(|s| record.get(s)))
            .unwrap_or("");
        let id = trim(id);
        rows.push(Row {
            audio_path: path,
            transcript: text.into(),
            speaker_id: if id.is_empty() {
                "unknown-speaker".into()
            } else {
                id.into()
            },
            row_number: i + 2,
        });
    }
    if rows.is_empty() {
        return Err(invalid("Common Voice metadata has no usable rows"));
    }
    Ok(rows)
}
fn rank(row: &Row, seed: i64) -> u64 {
    audio::deterministic_seed(&[
        &seed.to_string(),
        &row.audio_path,
        &row.speaker_id,
        &row.transcript,
    ])
}
fn round_robin(rows: &[Row], count: usize, seed: i64) -> Vec<Row> {
    let mut groups: Vec<(String, VecDeque<Row>)> = Vec::new();
    let mut indices = HashMap::new();
    for row in rows {
        let i = *indices.entry(row.speaker_id.clone()).or_insert_with(|| {
            groups.push((row.speaker_id.clone(), VecDeque::new()));
            groups.len() - 1
        });
        groups[i].1.push_back(row.clone());
    }
    groups.sort_by_key(|(speaker, _)| audio::deterministic_seed(&[&seed.to_string(), speaker]));
    for (_, rows) in &mut groups {
        rows.make_contiguous().sort_by_key(|r| rank(r, seed));
    }
    let mut selected = Vec::new();
    while selected.len() < count && !groups.is_empty() {
        for (_, rows) in &mut groups {
            if let Some(row) = rows.pop_front() {
                selected.push(row);
                if selected.len() == count {
                    break;
                }
            }
        }
        groups.retain(|(_, rows)| !rows.is_empty());
    }
    selected
}
pub fn select_rows(rows: &[Row], count: usize, seed: i64, min_speakers: usize) -> Result<Vec<Row>> {
    if count == 0 || count > rows.len() {
        return Err(invalid(format!(
            "requested {count} clips but {} available (count must be positive)",
            rows.len()
        )));
    }
    let mut selected = Vec::new();
    let mut paths = HashSet::new();
    for (i, bucket) in ["short", "medium", "long"].iter().enumerate() {
        let candidates: Vec<Row> = rows
            .iter()
            .filter(|r| r.length_bucket() == *bucket)
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Err(invalid(format!(
                "Common Voice subset has no usable {bucket} utterances"
            )));
        }
        let target = count / 3 + usize::from(i < count % 3);
        for row in round_robin(&candidates, target, seed) {
            if paths.insert(row.audio_path.clone()) {
                selected.push(row);
            }
        }
    }
    if selected.len() < count {
        let mut remaining: Vec<Row> = rows
            .iter()
            .filter(|r| !paths.contains(&r.audio_path))
            .cloned()
            .collect();
        remaining.sort_by_key(|r| rank(r, seed));
        selected.extend(remaining.into_iter().take(count - selected.len()));
    }
    if selected
        .iter()
        .map(|r| &r.speaker_id)
        .collect::<HashSet<_>>()
        .len()
        < min_speakers
    {
        return Err(invalid(format!(
            "selected subset contains fewer than {min_speakers} speakers"
        )));
    }
    Ok(selected)
}
pub fn resolve_dataset_path(root: &Path, relative: &str) -> Result<PathBuf> {
    if Path::new(relative).is_absolute() {
        return Err(invalid("metadata audio path must be relative"));
    }
    let root = root.canonicalize()?;
    let candidate = root.join(relative).canonicalize()?;
    if !candidate.starts_with(&root) {
        return Err(invalid(format!(
            "metadata path escapes dataset directory: {relative}"
        )));
    }
    if !candidate.is_file() {
        return Err(invalid("metadata audio path is not a file"));
    }
    Ok(candidate)
}
pub fn resolve_source(root: &Path, relative: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    if Path::new(relative).is_absolute() {
        return Err(invalid("Common Voice path must be relative"));
    }
    for base in [&root, &root.join("clips")] {
        let path = base.join(relative);
        // Reject escapes even if the parent is outside and the file is missing.
        let mut lexical = PathBuf::new();
        for part in path.components() {
            match part {
                std::path::Component::ParentDir => {
                    lexical.pop();
                }
                std::path::Component::CurDir => {}
                _ => lexical.push(part.as_os_str()),
            }
        }
        if !lexical.starts_with(&root) {
            return Err(invalid(format!(
                "Common Voice path escapes input directory: {relative}"
            )));
        }
        if path.exists() {
            let path = path.canonicalize()?;
            if !path.starts_with(&root) {
                return Err(invalid("Common Voice symlink escapes input directory"));
            }
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Err(invalid(format!("Common Voice clip not found: {relative}")))
}
#[derive(Debug, Clone)]
pub struct Prepare {
    pub input_dir: PathBuf,
    pub tsv: PathBuf,
    pub output_dir: PathBuf,
    pub environment_noise: PathBuf,
    pub count: usize,
    pub seed: i64,
    pub ffmpeg: String,
    pub source_name: String,
    pub source_url: String,
    pub license: String,
    pub dataset_version: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub id: String,
    pub speaker_id: String,
    pub length_bucket: String,
    pub condition: String,
    pub snr_db: Option<i32>,
    pub audio_path: String,
    pub transcript: String,
    pub duration_seconds: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub source_path: String,
    pub source_sha256: String,
    pub source_name: String,
    pub source_url: String,
    pub license: String,
}
/// Resolve the existing part of a destination before any file is created. This
/// also follows parent-directory symlinks when the destination does not exist.
pub(crate) fn resolved_destination(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut normalized = PathBuf::new();
    for part in absolute.components() {
        match part {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => normalized.push(part.as_os_str()),
        }
    }
    let mut ancestor = normalized.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| invalid("invalid output path"))?,
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| invalid("invalid output path"))?;
    }
    let mut resolved = ancestor.canonicalize()?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}
pub(crate) struct InputProtection {
    paths: HashSet<PathBuf>,
    handles: HashSet<same_file::Handle>,
}
impl InputProtection {
    pub(crate) fn new(inputs: Vec<PathBuf>) -> Result<Self> {
        let handles = inputs
            .iter()
            .map(same_file::Handle::from_path)
            .collect::<std::io::Result<_>>()?;
        Ok(Self {
            paths: inputs.into_iter().collect(),
            handles,
        })
    }
    pub(crate) fn check(&self, path: &Path) -> Result<PathBuf> {
        let resolved = resolved_destination(path)?;
        if self.paths.contains(&resolved)
            || (path.exists() && self.handles.contains(&same_file::Handle::from_path(path)?))
        {
            return Err(invalid(format!(
                "output would overwrite an input: {}",
                path.display()
            )));
        }
        Ok(resolved)
    }
}
pub fn prepare_dataset(config: &Prepare) -> Result<Value> {
    let root = config.input_dir.canonicalize()?;
    let tsv = config.tsv.canonicalize()?;
    let metadata_file = tsv
        .strip_prefix(&root)
        .map_err(|_| invalid("metadata TSV must be inside corpus root"))?
        .to_string_lossy()
        .replace('\\', "/");
    let selected = select_rows(&load_rows(&tsv)?, config.count, config.seed, 3)?;
    if !config.environment_noise.is_file() {
        return Err(invalid("environment noise file missing"));
    }
    // Resolve every selected input before writing any variant: a future clip
    // can otherwise be destroyed by an earlier clip's generated filename.
    let sources = selected
        .iter()
        .map(|row| resolve_source(&root, &row.audio_path))
        .collect::<Result<Vec<_>>>()?;
    let mut inputs = sources.clone();
    inputs.push(tsv.clone());
    inputs.push(config.environment_noise.canonicalize()?);
    let protection = InputProtection::new(inputs)?;
    std::fs::create_dir_all(&config.output_dir)?;
    let output_root = config.output_dir.canonicalize()?;
    let mut targets = vec!["metadata.csv".to_owned(), "manifest.json".to_owned()];
    for i in 0..selected.len() {
        for condition in std::iter::once("clean").chain(CONDITIONS.iter().map(|(_, _, c)| *c)) {
            targets.push(format!("{condition}/cv_{:04}.wav", i + 1));
        }
    }
    for target in targets {
        let destination = config.output_dir.join(target);
        if !protection.check(&destination)?.starts_with(&output_root) {
            return Err(invalid("generated audio path escapes output directory"));
        }
    }
    let temp = tempfile::tempdir()?;
    let environment = audio::convert_to_standard_wav(
        &config.environment_noise,
        &temp.path().join("environment.wav"),
        &config.ffmpeg,
    )?;
    if environment.is_empty() {
        return Err(invalid("environment noise is empty"));
    }
    let mut metadata = Vec::new();
    let mut items = Vec::new();
    let mut total = 0.;
    for (i, row) in selected.iter().enumerate() {
        let id = format!("cv_{:04}", i + 1);
        let source = &sources[i];
        let digest = audio::sha256_file(source)?;
        let clean = audio::convert_to_standard_wav(
            source,
            &config.output_dir.join(format!("clean/{id}.wav")),
            &config.ffmpeg,
        )?;
        let duration = clean.len() as f64 / audio::SAMPLE_RATE as f64;
        total += duration;
        let mut variants = Vec::new();
        for (kind, snr, condition) in std::iter::once(("clean", None, "clean"))
            .chain(CONDITIONS.iter().map(|(k, s, c)| (*k, Some(*s), *c)))
        {
            let path = format!("{condition}/{id}.wav");
            if let Some(snr) = snr {
                let samples = audio::make_noise_variant(
                    &clean,
                    kind,
                    snr as f64,
                    audio::deterministic_seed(&[&config.seed.to_string(), &id, condition]),
                    Some(&environment),
                )?;
                audio::write_pcm16_wav(
                    &config.output_dir.join(&path),
                    &samples,
                    audio::SAMPLE_RATE,
                )?;
            }
            variants.push(json!({"condition":condition,"snr_db":snr,"audio_path":path}));
            metadata.push(Metadata {
                id: id.clone(),
                speaker_id: row.speaker_id.clone(),
                length_bucket: row.length_bucket().into(),
                condition: condition.into(),
                snr_db: snr,
                audio_path: path,
                transcript: row.transcript.clone(),
                duration_seconds: format!("{duration:.6}"),
                sample_rate: audio::SAMPLE_RATE,
                channels: 1,
                source_path: row.audio_path.clone(),
                source_sha256: digest.clone(),
                source_name: config.source_name.clone(),
                source_url: config.source_url.clone(),
                license: config.license.clone(),
            });
        }
        items.push(json!({"id":id,"speaker_id":row.speaker_id,"length_bucket":row.length_bucket(),"transcript":row.transcript,"source_path":row.audio_path,"source_sha256":digest,"variants":variants}));
    }
    let mut writer = csv::Writer::from_path(config.output_dir.join("metadata.csv"))?;
    for row in metadata {
        writer.serialize(row)?;
    }
    writer.flush()?;
    let manifest = json!({"schema_version":1,"seed":config.seed,"count":selected.len(),"total_clean_duration_seconds":(total*1e6).round_ties_even()/1e6,"audio_format":{"sample_rate":audio::SAMPLE_RATE,"channels":1,"sample_width":2,"encoding":"PCM signed 16-bit little-endian"},"source":{"name":config.source_name,"url":config.source_url,"license":config.license,"dataset_version":config.dataset_version,"metadata_file":metadata_file},"environment_noise":{"name":config.environment_noise.file_name().unwrap_or_default().to_string_lossy(),"sha256":audio::sha256_file(&config.environment_noise)?},"noise_algorithm":audio::NOISE_ALGORITHM,"noise_variants":CONDITIONS.iter().map(|(k,s,c)|json!({"condition":c,"kind":k,"snr_db":s})).collect::<Vec<_>>(),"items":items});
    std::fs::write(
        config.output_dir.join("manifest.json"),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    Ok(manifest)
}
