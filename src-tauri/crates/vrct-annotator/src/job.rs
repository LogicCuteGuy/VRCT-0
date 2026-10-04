use crate::{filesystem as fsx, random::Random, Result};
use chrono::Utc;
use image::ImageDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

pub const DEFAULT_MODEL: &str = "gemini-2.5-flash";
pub const PROMPT: &str = include_str!("../assets/prompt.txt");
pub const LABEL_CONFIG: &str = "<View>\n  <Image name=\"image\" value=\"$image\" zoom=\"true\" zoomControl=\"true\" rotateControl=\"false\"/>\n  <RectangleLabels name=\"chatbox\" toName=\"image\" canRotate=\"false\">\n    <Label value=\"chat_box\" background=\"#00AA88\"/>\n  </RectangleLabels>\n</View>\n";
pub fn schema() -> Value {
    json!({"type":"array", "items":{"type":"object","properties":{"box_2d":{"type":"array","items":{"type":"integer","minimum":0,"maximum":1000},"minItems":4,"maxItems":4},"label":{"type":"string","enum":["chat_box"]}},"required":["box_2d","label"],"additionalProperties":false}})
}

/// Python json.dumps(sort_keys=True) spacing. The policy consists entirely of
/// ASCII strings, so this matches the existing job's SHA-256 exactly.
fn policy_json(value: &Value) -> String {
    match value {
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(policy_json).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.iter()
                    .map(|key| format!(
                        "{}: {}",
                        serde_json::to_string(key).unwrap(),
                        policy_json(&object[*key])
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        _ => serde_json::to_string(value).unwrap(),
    }
}
pub fn policy_hash(model: &str) -> String {
    hex::encode(Sha256::digest(
        policy_json(&json!([model, PROMPT, schema()])).as_bytes(),
    ))
}
pub fn valid_model(model: &str) -> bool {
    model.strip_prefix("gemini-").is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-'))
    })
}
pub fn utc_now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}
pub fn unique_stamp() -> Result<String> {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).map_err(|_| "random identifier unavailable")?;
    Ok(format!(
        "{}_{}",
        Utc::now().format("%Y%m%dT%H%M%S_%fZ"),
        hex::encode(bytes)
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub image: String,
    pub source: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub capture: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub created_at: String,
    pub model: String,
    pub policy_hash: String,
    pub seed: i64,
    pub source_root: String,
    pub available_images: usize,
    pub images: Vec<Entry>,
}
#[derive(Clone)]
struct Candidate {
    path: PathBuf,
    source: String,
    capture: Value,
}

pub fn validate_boxes(value: &Value) -> Result<()> {
    let boxes = value.as_array().ok_or("Expected an array of boxes")?;
    let mut seen = HashSet::new();
    for object in boxes {
        let object = object.as_object().ok_or("Invalid box object or label")?;
        if object.len() != 2
            || !object.contains_key("box_2d")
            || object.get("label").and_then(Value::as_str) != Some("chat_box")
        {
            return Err("Invalid box object or label".into());
        }
        let values = object["box_2d"]
            .as_array()
            .filter(|v| v.len() == 4)
            .ok_or("box_2d must contain four integers")?;
        let mut box2d = [0u16; 4];
        for (index, value) in values.iter().enumerate() {
            box2d[index] = value
                .as_u64()
                .filter(|n| *n <= 1000)
                .ok_or("box_2d must contain four integers within 0..1000")?
                as u16;
        }
        if box2d[0] >= box2d[2] || box2d[1] >= box2d[3] {
            return Err("Box must have positive area within 0..1000".into());
        }
        if !seen.insert(box2d) {
            return Err("Duplicate box".into());
        }
    }
    Ok(())
}

fn discover(root: &Path, directory: &Path, candidates: &mut Vec<Candidate>) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|e| e.file_name());
    // os.walk processes this directory's files before descending into children.
    for entry in &entries {
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            continue;
        }
        if !path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
            || !path
                .parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str())
                .is_some_and(|n| matches!(n, "unlabeled" | "positive" | "negative"))
        {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "input path escaped root")?
            .to_str()
            .ok_or("input path is not UTF-8")?
            .replace('\\', "/");
        let path = fsx::safe(root, &relative)?;
        let sidecar = path.with_extension("json");
        let metadata_name = sidecar
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        let sidecar = fsx::safe(root, &metadata_name)?;
        let data: Value = fsx::json(&sidecar)?;
        if data["image"].as_str() != path.file_name().and_then(|n| n.to_str())
            || data["label"].as_str()
                != path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|n| n.to_str())
            || ["session", "run_id"]
                .iter()
                .any(|key| data[*key].as_str().is_none_or(str::is_empty))
            || ["width", "height"].iter().any(|key| {
                data[*key]
                    .as_u64()
                    .is_none_or(|n| n == 0 || n > u32::MAX as u64)
            })
        {
            return Err(format!("Invalid collector metadata: {relative}"));
        }
        candidates.push(Candidate {
            path,
            source: relative,
            capture: data,
        });
    }
    for entry in entries {
        let path = entry.path();
        if entry.file_type().map_err(|e| e.to_string())?.is_dir()
            && fs::canonicalize(&path).map_err(|e| e.to_string())? == path
        {
            discover(root, &path, candidates)?;
        }
    }
    Ok(())
}

fn select(candidates: Vec<Candidate>, limit: usize, seed: i64) -> Vec<Candidate> {
    type Group = (String, String, String);
    let mut groups: BTreeMap<Group, (usize, Vec<Candidate>)> = BTreeMap::new();
    for candidate in candidates {
        let key = (
            candidate.capture["session"].as_str().unwrap().to_owned(),
            candidate.capture["run_id"].as_str().unwrap().to_owned(),
            candidate.capture["backend"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
        );
        let order = groups.len();
        groups
            .entry(key)
            .or_insert_with(|| (order, Vec::new()))
            .1
            .push(candidate);
    }
    let mut rng = Random::new(seed);
    let mut keys = groups.keys().cloned().collect::<Vec<_>>();
    rng.shuffle(&mut keys);
    let mut order = groups
        .iter()
        .map(|(key, (index, _))| (*index, key.clone()))
        .collect::<Vec<_>>();
    order.sort();
    for (_, key) in order {
        rng.shuffle(&mut groups.get_mut(&key).unwrap().1);
    }
    let mut selected = Vec::new();
    while !keys.is_empty() && (limit == 0 || selected.len() < limit) {
        for key in keys.clone() {
            let values = &mut groups.get_mut(&key).unwrap().1;
            selected.push(values.pop().unwrap());
            if values.is_empty() {
                keys.retain(|k| *k != key);
            }
            if limit != 0 && selected.len() == limit {
                break;
            }
        }
    }
    selected
}

fn resolve_new(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    let mut ancestor = absolute.as_path();
    let mut tail = Vec::new();
    while !ancestor.exists() {
        tail.push(
            ancestor
                .file_name()
                .ok_or("invalid output path")?
                .to_owned(),
        );
        ancestor = ancestor.parent().ok_or("invalid output parent")?;
    }
    let mut output = fs::canonicalize(ancestor).map_err(|e| e.to_string())?;
    for part in tail.into_iter().rev() {
        fsx::relative(part.to_str().ok_or("output path is not UTF-8")?)?;
        output.push(part);
    }
    Ok(output)
}

pub fn prepare(
    input: &Path,
    output: &Path,
    limit: usize,
    seed: i64,
    model: &str,
) -> Result<Manifest> {
    let input = fsx::root(input)?;
    let output = resolve_new(output)?;
    if !valid_model(model) {
        return Err("Invalid Gemini model".into());
    }
    if output.exists() || output.starts_with(&input) {
        return Err("Choose a NEW job directory outside the input folder".into());
    }
    let mut candidates = Vec::new();
    discover(&input, &input, &mut candidates)?;
    if candidates.is_empty() {
        return Err(
            "No collector PNG/JSON pairs found under unlabeled/, positive/ or negative/".into(),
        );
    }
    let available_images = candidates.len();
    let selected = select(candidates, limit, seed);
    let parent = output.parent().ok_or("invalid output parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".prepare_")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let stage = temporary.path().join("job");
    fs::create_dir_all(stage.join("images")).map_err(|e| e.to_string())?;
    let stage = fsx::root(&stage)?;
    let mut images = Vec::new();
    for candidate in selected {
        let id = hex::encode(Sha256::digest(candidate.source.as_bytes()))[..24].to_owned();
        let image = format!("images/{id}.png");
        let path = fsx::safe(&stage, &image)?;
        if path.exists() {
            return Err("Image identifier collision".into());
        }
        fs::copy(&candidate.path, &path).map_err(|e| e.to_string())?;
        let width = candidate.capture["width"].as_u64().unwrap() as u32;
        let height = candidate.capture["height"].as_u64().unwrap() as u32;
        let mut decoder = image::codecs::png::PngDecoder::new(BufReader::new(
            File::open(&path).map_err(|e| e.to_string())?,
        ))
        .map_err(|_| "Invalid PNG image")?;
        decoder
            .set_limits(image::Limits::default())
            .map_err(|_| "PNG image exceeds decoding limits")?;
        if decoder.dimensions() != (width, height) {
            return Err("Image dimensions differ from metadata".into());
        }
        if decoder
            .orientation()
            .map_err(|_| "Invalid PNG orientation")?
            != image::metadata::Orientation::NoTransforms
        {
            return Err("Rotated image is unsupported".into());
        }
        let _ = image::DynamicImage::from_decoder(decoder).map_err(|_| "Invalid PNG image")?;
        // Reading the complete decoder validates the image, not just its header.
        images.push(Entry {
            id,
            image,
            source: candidate.source,
            sha256: fsx::hash(&path)?,
            width,
            height,
            capture: candidate.capture,
        });
    }
    let manifest = Manifest {
        format_version: 1,
        created_at: utc_now(),
        model: model.into(),
        policy_hash: policy_hash(model),
        seed,
        source_root: fsx::display_path(&input),
        available_images,
        images,
    };
    fsx::write_json(&stage, "manifest.json", &manifest)?;
    fsx::atomic(&stage, "prompt.txt", PROMPT.as_bytes())?;
    fsx::write_json(&stage, "response_schema.json", &schema())?;
    fs::rename(&stage, &output).map_err(|e| format!("job publication failed: {e}"))?;
    Ok(manifest)
}

pub fn lock(job: &Path) -> Result<fsx::Lock> {
    fsx::Lock::new(&fsx::root(job)?, ".lock")
}
pub fn load(job: &Path) -> Result<Manifest> {
    let job = fsx::root(job)?;
    let manifest: Manifest = fsx::json(&fsx::safe(&job, "manifest.json")?)?;
    if manifest.format_version != 1 || manifest.images.is_empty() || !valid_model(&manifest.model) {
        return Err("Unsupported/incomplete annotation job".into());
    }
    if manifest.policy_hash != policy_hash(&manifest.model) {
        return Err(
            "Model/prompt/schema changed. Prepare a new job; existing results are preserved".into(),
        );
    }
    let mut seen = HashSet::new();
    for entry in &manifest.images {
        let hex = |text: &str, n| {
            text.len() == n
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        };
        if !hex(&entry.id, 24)
            || !seen.insert(&entry.id)
            || entry.image != format!("images/{}.png", entry.id)
            || !hex(&entry.sha256, 64)
            || entry.width == 0
            || entry.height == 0
            || ["session", "run_id"]
                .iter()
                .any(|k| entry.capture[*k].as_str().is_none_or(str::is_empty))
        {
            return Err("Invalid job image entry".into());
        }
        fsx::relative(&entry.source)?;
    }
    Ok(manifest)
}
pub fn success(status: &str) -> bool {
    matches!(status, "detected" | "no_detection")
}
pub fn read_result(job: &Path, entry: &Entry, manifest: &Manifest) -> Result<Value> {
    let job = fsx::root(job)?;
    let path = fsx::safe(&job, &format!("results/{}.json", entry.id))?;
    if !path.exists() {
        return Ok(json!({"status":"pending"}));
    }
    let result: Value = fsx::json(&path)?;
    let status = result["status"].as_str().ok_or("Invalid result status")?;
    if result["image_sha256"] != entry.sha256
        || result["policy_hash"] != manifest.policy_hash
        || !matches!(
            status,
            "detected" | "no_detection" | "api_error" | "invalid_response" | "unknown" | "pending"
        )
    {
        return Err(format!("Result does not match image/policy: {}", entry.id));
    }
    if success(status) {
        validate_boxes(&result["boxes"])?;
        if result["boxes"].as_array().unwrap().is_empty() == (status == "detected") {
            return Err("Result status and boxes disagree".into());
        }
    }
    Ok(result)
}
pub fn verify_images(job: &Path, manifest: &Manifest) -> Result<()> {
    let job = fsx::root(job)?;
    for entry in &manifest.images {
        if fsx::hash(&fsx::safe(&job, &entry.image)?)? != entry.sha256 {
            return Err(format!("Job image changed or missing: {}", entry.id));
        }
    }
    Ok(())
}

pub fn task(entry: &Entry, result: &Value, model: &str) -> Value {
    let mut task = json!({"data":{"image":format!("/data/local-files/?d={}",entry.image),"image_id":entry.id,"status":result["status"],"source":entry.source,"session":entry.capture["session"],"run_id":entry.capture["run_id"]}});
    if success(result["status"].as_str().unwrap_or("")) {
        let regions = result["boxes"].as_array().unwrap().iter().enumerate().map(|(index,object)| {
            let b = object["box_2d"].as_array().unwrap(); let n = |i: usize| b[i].as_f64().unwrap();
            json!({"id":format!("{}_{index}",entry.id),"type":"rectanglelabels","from_name":"chatbox","to_name":"image","original_width":entry.width,"original_height":entry.height,"image_rotation":0,"value":{"x":n(1)/10.,"y":n(0)/10.,"width":(n(3)-n(1))/10.,"height":(n(2)-n(0))/10.,"rotation":0,"rectanglelabels":["chat_box"]}})
        }).collect::<Vec<_>>();
        task["predictions"] = json!([{"model_version":model,"result":regions}]);
    }
    task
}
pub(crate) fn export_unlocked(job: &Path, manifest: &Manifest) -> Result<PathBuf> {
    let job = fsx::root(job)?;
    verify_images(&job, manifest)?;
    let tasks = manifest
        .images
        .iter()
        .map(|entry| {
            Ok(task(
                entry,
                &read_result(&job, entry, manifest)?,
                &manifest.model,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let exports = fsx::safe(&job, "exports")?;
    fs::create_dir_all(&exports).map_err(|e| e.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".export_")
        .tempdir_in(&exports)
        .map_err(|e| e.to_string())?;
    let stage = fsx::root(temporary.path())?;
    fsx::write_json(&stage, "tasks.json", &tasks)?;
    fsx::atomic(&stage, "label_config.xml", LABEL_CONFIG.as_bytes())?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for task in &tasks {
        *counts
            .entry(task["data"]["status"].as_str().unwrap().into())
            .or_default() += 1;
    }
    fsx::write_json(
        &stage,
        "summary.json",
        &json!({"counts":counts,"model":manifest.model,"all_tasks_require_human_review":true}),
    )?;
    let start = format!("\u{feff}Label Studio / Windows\n\n1. Use your separate Label Studio installation. In PowerShell:\n$env:LABEL_STUDIO_LOCAL_FILES_SERVING_ENABLED = 'true'\n$env:LABEL_STUDIO_LOCAL_FILES_DOCUMENT_ROOT = '{}'\nlabel-studio start\n\n2. Create a NEW project; paste label_config.xml into its Labeling Interface.\n3. Settings > Cloud Storage > Add Source Storage > Local Files: choose {}. Import Method: Tasks, empty filter, Save only. Do NOT Save & Sync.\n4. Import tasks.json once in this new project.\n5. Enable Show predictions to annotators. Copy predictions into annotations, correct boxes and Submit EVERY image, including no_detection, failed and pending images.\n6. Export YOLO only after human review. Confirm chat_box has class ID 0. Retain reviewed empty labels as negative samples; never treat unreviewed/Skipped images as negatives.\n\nPredictions are provisional, not confirmed annotations. Keep the complete job; update DOCUMENT_ROOT after moving it. Use separate Label Studio launch environments for separate jobs.\n",fsx::display_path(&job).replace('\'',"''"),fsx::display_path(&job.join("images")));
    fsx::atomic(&stage, "START_HERE.txt", start.as_bytes())?;
    let destination = exports.join(unique_stamp()?);
    fs::rename(&stage, &destination).map_err(|e| e.to_string())?;
    Ok(destination)
}
pub fn export(job: &Path) -> Result<PathBuf> {
    let _lock = lock(job)?;
    export_unlocked(job, &load(job)?)
}
pub(crate) fn status_unlocked(job: &Path, manifest: &Manifest) -> Result<Value> {
    let job = fsx::root(job)?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut usage: BTreeMap<String, i64> = BTreeMap::new();
    for entry in &manifest.images {
        *counts
            .entry(
                read_result(&job, entry, manifest)?["status"]
                    .as_str()
                    .unwrap()
                    .into(),
            )
            .or_default() += 1;
        let attempts = fsx::safe(&job, &format!("attempts/{}", entry.id))?;
        if !attempts.exists() {
            continue;
        }
        for file in fs::read_dir(attempts).map_err(|e| e.to_string())? {
            let file = file.map_err(|e| e.to_string())?;
            if file.path().extension().is_none_or(|e| e != "json") {
                continue;
            }
            let name = format!(
                "attempts/{}/{}",
                entry.id,
                file.file_name()
                    .to_str()
                    .ok_or("invalid attempt filename")?
            );
            let record: Value = fsx::json(&fsx::safe(&job, &name)?)?;
            if let Some(fields) = record["response"]["usage"].as_object() {
                for (field, value) in fields {
                    if field.ends_with("token_count") {
                        if let Some(value) = value.as_i64() {
                            let count = usage.entry(field.clone()).or_default();
                            *count = count.checked_add(value).ok_or("usage counter overflow")?;
                        }
                    }
                }
            }
        }
    }
    Ok(
        json!({"images":manifest.images.len(),"model":manifest.model,"counts":counts,"reported_usage":usage}),
    )
}
pub fn status(job: &Path) -> Result<Value> {
    let _lock = lock(job)?;
    status_unlocked(job, &load(job)?)
}
