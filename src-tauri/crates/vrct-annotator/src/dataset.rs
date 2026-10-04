use crate::{filesystem as fsx, random::Random, Result};
use image::ImageDecoder;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Options {
    pub val_ratio: f64,
    pub seed: i64,
    pub scene_size: u64,
    pub refreeze: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            val_ratio: 0.2,
            seed: 0,
            scene_size: 10,
            refreeze: false,
        }
    }
}
#[derive(Debug)]
pub struct Summary {
    pub train: usize,
    pub val: usize,
    pub positives: usize,
    pub negatives: usize,
}
struct Item {
    image: String,
    label: String,
    body: String,
    scene: String,
    image_sha256: String,
}

pub fn normalize_label(text: &str) -> Result<String> {
    let mut normalized = String::new();
    for line in text.replace("\\n", "\n").lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.is_empty() {
            continue;
        }
        if fields.len() != 5 {
            return Err("broken label (expected 5 fields)".into());
        }
        let class = fields[0]
            .parse::<u64>()
            .map_err(|_| "broken label (not a number or negative class)")?;
        let coordinates = fields[1..]
            .iter()
            .map(|field| {
                field
                    .parse::<f64>()
                    .map_err(|_| "broken label (not a number)".to_owned())
            })
            .collect::<Result<Vec<_>>>()?;
        if coordinates
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err("broken label (out of range)".into());
        }
        normalized.push_str(&format!(
            "{class} {:.6} {:.6} {:.6} {:.6}\n",
            coordinates[0], coordinates[1], coordinates[2], coordinates[3]
        ));
    }
    Ok(normalized)
}
pub fn scene_of(session: &str, stem: &str, scene_size: u64) -> Result<String> {
    if scene_size == 0 {
        return Err("--scene-size must be 1 or more".into());
    }
    if let Some((run, index)) = stem.rsplit_once('_') {
        if !run.is_empty() && !index.is_empty() && index.bytes().all(|c| c.is_ascii_digit()) {
            let index = index
                .parse::<u64>()
                .map_err(|_| "frame index exceeds supported integer range")?;
            return Ok(format!("{session}/{run}#{:04}", index / scene_size));
        }
    }
    Ok(format!("{session}/{stem}"))
}
fn sorted(directory: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}
fn discover(root: &Path, scene_size: u64) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    let mut sessions = 0;
    for session in sorted(root)? {
        if !session.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let name = session
            .file_name()
            .into_string()
            .map_err(|_| "session name is not UTF-8")?;
        let directory = fsx::safe(root, &name)?;
        if !directory.join("images").is_dir() || !directory.join("annotations").is_dir() {
            continue;
        }
        sessions += 1;
        let images = fsx::safe(root, &format!("{name}/images"))?;
        fsx::safe(root, &format!("{name}/annotations"))?;
        for image in sorted(&images)? {
            let path = image.path();
            if !path
                .extension()
                .and_then(|p| p.to_str())
                .is_some_and(|extension| {
                    ["png", "jpg", "jpeg"]
                        .iter()
                        .any(|s| extension.eq_ignore_ascii_case(s))
                })
            {
                continue;
            }
            let filename = image
                .file_name()
                .into_string()
                .map_err(|_| "image filename is not UTF-8")?;
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("invalid image stem")?;
            let relative_image = format!("{name}/images/{filename}");
            let source = fsx::safe(root, &format!("{name}/annotations/{stem}.txt"))?;
            let path = fsx::safe(root, &relative_image)?;
            if !source.is_file() {
                eprintln!("skip (no label): {filename}");
                continue;
            }
            let text = fs::read_to_string(source).map_err(|e| e.to_string())?;
            let body =
                normalize_label(&text).map_err(|error| format!("{error}: {name}/{stem}.txt"))?;
            items.push(Item {
                image: format!("./{relative_image}"),
                label: format!("{name}/labels/{stem}.txt"),
                body,
                scene: scene_of(&name, stem, scene_size)?,
                image_sha256: fsx::hash(&path)?,
            });
        }
    }
    if sessions == 0 {
        return Err("no annotated session under dataset root".into());
    }
    if items.is_empty() {
        return Err("no reviewed image/label pairs under dataset root".into());
    }
    Ok(items)
}
fn split(items: &[Item], options: &Options) -> Result<(Vec<String>, Vec<String>)> {
    let mut scenes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for item in items {
        scenes
            .entry(item.scene.clone())
            .or_default()
            .push(item.image.clone());
    }
    let mut sessions: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for scene in scenes.keys() {
        sessions
            .entry(scene.split('/').next().unwrap())
            .or_default()
            .push(scene);
    }
    let mut val_scenes = BTreeSet::new();
    for (session, mut keys) in sessions {
        Random::new(options.seed).shuffle(&mut keys);
        let target = (keys.iter().map(|key| scenes[*key].len()).sum::<usize>() as f64
            * options.val_ratio)
            .round_ties_even() as usize;
        let mut taken = 0;
        let mut selected = 0;
        for key in &keys {
            if taken >= target {
                break;
            }
            val_scenes.insert(*key);
            taken += scenes[*key].len();
            selected += 1;
        }
        if selected == 0 || selected == keys.len() {
            return Err(format!("cannot split {session} ({} scene(s)) with --val-ratio {}: reduce --scene-size or collect more scenes",keys.len(),options.val_ratio));
        }
    }
    let mut train = Vec::new();
    let mut val = Vec::new();
    for item in items {
        if val_scenes.contains(item.scene.as_str()) {
            val.push(item.image.clone());
        } else {
            train.push(item.image.clone());
        }
    }
    train.sort();
    val.sort();
    Ok((train, val))
}

pub fn prepare(root: &Path, options: &Options) -> Result<Summary> {
    if !options.val_ratio.is_finite()
        || !(0.0..1.0).contains(&options.val_ratio)
        || options.val_ratio == 0.
    {
        return Err("--val-ratio must be between 0 and 1".into());
    }
    if options.scene_size == 0 {
        return Err("--scene-size must be 1 or more".into());
    }
    let root = fsx::root(root)?;
    let _lock = fsx::Lock::new(&root, ".dataset.lock")?;
    let items = discover(&root, options.scene_size)?;
    let frozen = fsx::safe(&root, "val_fixed.txt")?;
    let old_manifest_path = fsx::safe(&root, "splits_manifest.json")?;
    let previous: Option<Value> = if old_manifest_path.exists() && !options.refreeze {
        Some(fsx::json(&old_manifest_path)?)
    } else {
        None
    };
    let reuse = frozen.is_file() && !options.refreeze;
    let (train, val) = if reuse {
        if let Some(previous) = &previous {
            if previous["format_version"] != 1 || !previous["frozen_images"].is_object() {
                return Err(
                    "invalid frozen split manifest; restore it or explicitly --refreeze".into(),
                );
            }
            if previous["frozen_val_sha256"].as_str() != Some(fsx::hash(&frozen)?.as_str()) {
                return Err(
                    "val_fixed.txt changed; use --refreeze to change the evaluation baseline"
                        .into(),
                );
            }
        }
        let val = fs::read_to_string(&frozen)
            .map_err(|e| e.to_string())?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let known = items
            .iter()
            .map(|item| item.image.clone())
            .collect::<BTreeSet<_>>();
        if val.iter().any(|entry| !known.contains(entry)) {
            return Err("val_fixed.txt references a missing image/label pair".into());
        }
        for item in items.iter().filter(|item| val.contains(&item.image)) {
            if let Some(previous) = &previous {
                let hash = previous["frozen_images"][&item.image].as_str().ok_or(
                    "frozen image checksum missing; restore manifest or explicitly --refreeze",
                )?;
                if hash != item.image_sha256 {
                    return Err(
                        "frozen validation image changed; restore it or explicitly --refreeze"
                            .into(),
                    );
                }
            }
        }
        let train = known.difference(&val).cloned().collect::<Vec<_>>();
        (train, val.into_iter().collect::<Vec<_>>())
    } else {
        split(&items, options)?
    };
    if train.is_empty() || val.is_empty() {
        return Err("cannot split: train and validation must both contain images".into());
    }
    let val_set = val.iter().collect::<BTreeSet<_>>();
    let train_scenes = items
        .iter()
        .filter(|item| !val_set.contains(&item.image))
        .map(|item| &item.scene)
        .collect::<BTreeSet<_>>();
    if items
        .iter()
        .filter(|item| val_set.contains(&item.image))
        .any(|item| train_scenes.contains(&item.scene))
    {
        return Err(
            "frozen validation splits a scene; use a new capture run or explicitly --refreeze"
                .into(),
        );
    }
    // All input validation and split decisions precede the first label write.
    for item in &items {
        let target = fsx::safe(&root, &item.label)?;
        if !target.is_file() || fs::read_to_string(&target).map_err(|e| e.to_string())? != item.body
        {
            fsx::atomic(&root, &item.label, item.body.as_bytes())?;
        }
    }
    let list = |entries: &[String]| format!("{}\n", entries.join("\n"));
    if !reuse {
        fsx::atomic(&root, "val_fixed.txt", list(&val).as_bytes())?;
    }
    fsx::atomic(&root, "train.txt", list(&train).as_bytes())?;
    fsx::atomic(&root, "val.txt", list(&val).as_bytes())?;
    let path = fsx::display_path(&root).replace('\\', "/");
    let yaml = format!(
        "path: {}\ntrain: train.txt\nval: val.txt\nnames:\n  0: chat\n",
        serde_json::to_string(&path).map_err(|e| e.to_string())?
    );
    fsx::atomic(&root, "data.yaml", yaml.as_bytes())?;
    let frozen_images = items
        .iter()
        .filter(|item| val_set.contains(&item.image))
        .map(|item| (item.image.clone(), json!(item.image_sha256)))
        .collect::<serde_json::Map<_, _>>();
    let inventory = items.iter().map(|item| (item.image.clone(),json!({"scene":item.scene,"image_sha256":item.image_sha256,"label_sha256":hex::encode(Sha256::digest(item.body.as_bytes())),"positive":!item.body.is_empty()}))).collect::<serde_json::Map<_,_>>();
    let parameters = if reuse {
        previous
            .as_ref()
            .map(|m| m["freeze_parameters"].clone())
            .unwrap_or(Value::Null)
    } else {
        json!({"seed":options.seed,"scene_size":options.scene_size,"val_ratio":options.val_ratio})
    };
    fsx::write_json(
        &root,
        "splits_manifest.json",
        &json!({"format_version":1,"frozen_val_sha256":fsx::hash(&frozen)?,"freeze_parameters":parameters,"frozen_images":frozen_images,"images":inventory,"train":train,"val":val}),
    )?;
    let positives = items.iter().filter(|item| !item.body.is_empty()).count();
    Ok(Summary {
        train: train.len(),
        val: val.len(),
        positives,
        negatives: items.len() - positives,
    })
}

pub fn build_coco(root: &Path, split: &str) -> Result<Value> {
    if !matches!(split, "train" | "val") {
        return Err("COCO split must be train or val".into());
    }
    let root = fsx::root(root)?;
    let listing = fsx::safe(&root, &format!("{split}.txt"))?;
    let text = fs::read_to_string(listing)
        .map_err(|_| "split listing not found; run vrct-dataset prepare first")?;
    let mut images = Vec::new();
    let mut annotations = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        let entry = line.trim();
        if entry.is_empty() {
            continue;
        }
        let relative = entry.strip_prefix("./").unwrap_or(entry);
        if !seen.insert(relative.to_owned()) {
            return Err("duplicate image in COCO split".into());
        }
        let components = relative.split('/').collect::<Vec<_>>();
        if components.len() != 3 || components[1] != "images" {
            return Err("COCO image must be a session/images/file relative path".into());
        }
        let path = fsx::safe(&root, relative)?;
        let mut decoder = image::ImageReader::open(&path)
            .map_err(|_| "unreadable image")?
            .with_guessed_format()
            .map_err(|_| "unreadable image")?
            .into_decoder()
            .map_err(|_| "unreadable image")?;
        let orientation = decoder
            .orientation()
            .map_err(|_| "unreadable image orientation")?;
        let mut image =
            image::DynamicImage::from_decoder(decoder).map_err(|_| "unreadable image")?;
        image.apply_orientation(orientation);
        let (width, height) = (image.width(), image.height());
        let image_id = index + 1;
        images.push(json!({"id":image_id,"file_name":relative,"width":width,"height":height}));
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("invalid image filename")?;
        let label = fsx::safe(&root, &format!("{}/labels/{stem}.txt", components[0]))?;
        let body = normalize_label(&fs::read_to_string(label).map_err(|_| "missing label")?)?;
        for row in body.lines() {
            let fields = row.split_whitespace().collect::<Vec<_>>();
            let class = fields[0].parse::<u64>().map_err(|_| "invalid class ID")?;
            let c = fields[1..]
                .iter()
                .map(|field| {
                    field
                        .parse::<f64>()
                        .map_err(|_| "invalid box coordinate".to_owned())
                })
                .collect::<Result<Vec<_>>>()?;
            let x = (c[0] - c[2] / 2.) * f64::from(width);
            let y = (c[1] - c[3] / 2.) * f64::from(height);
            let w = c[2] * f64::from(width);
            let h = c[3] * f64::from(height);
            annotations.push(json!({"id":annotations.len()+1,"image_id":image_id,"category_id":class.checked_add(1).ok_or("class ID overflow")?,"bbox":[x,y,w,h],"area":w*h,"iscrowd":0}));
        }
    }
    Ok(
        json!({"images":images,"annotations":annotations,"categories":[{"id":1,"name":"chat","supercategory":"chat"}]}),
    )
}
pub fn coco(root: &Path) -> Result<Vec<PathBuf>> {
    let root = fsx::root(root)?;
    let _lock = fsx::Lock::new(&root, ".dataset.lock")?;
    let train = build_coco(&root, "train")?;
    let val = build_coco(&root, "val")?;
    let training_images = train["images"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|image| image["file_name"].as_str())
        .collect::<BTreeSet<_>>();
    if val["images"]
        .as_array()
        .unwrap()
        .iter()
        .any(|image| training_images.contains(image["file_name"].as_str().unwrap()))
    {
        return Err("an image occurs in both train and validation COCO splits".into());
    }
    let mut output = Vec::new();
    for (split, value) in [("train", train), ("val", val)] {
        let name = format!("annotations/instances_{split}_chatbox.json");
        fsx::write_json(&root, &name, &value)?;
        output.push(root.join(name));
    }
    Ok(output)
}
