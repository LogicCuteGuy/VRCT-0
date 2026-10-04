use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use vrct_annotator::dataset::{self, Options};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("データ set");
        fs::create_dir(&root).unwrap();
        Self {
            _directory: directory,
            root,
        }
    }
    fn session(&self, name: &str, frames: usize, negatives: &[usize], real_png: bool) -> PathBuf {
        let session = self.root.join(name);
        fs::create_dir_all(session.join("images")).unwrap();
        fs::create_dir_all(session.join("annotations")).unwrap();
        for i in 0..frames {
            let stem = format!("{name}_run_{i:06}");
            let path = session.join("images").join(format!("{stem}.png"));
            if real_png {
                image::RgbImage::from_pixel(200, 100, image::Rgb([0, 0, 0]))
                    .save(path)
                    .unwrap();
            } else {
                fs::write(path, b"fake image for split-only tests").unwrap();
            }
            let body = if negatives.contains(&i) {
                String::new()
            } else {
                format!("0 0.5 0.5 0.1 0.1{}", if i % 2 == 0 { "\\n" } else { "\n" })
            };
            fs::write(
                session.join("annotations").join(format!("{stem}.txt")),
                body,
            )
            .unwrap();
        }
        session
    }
}
fn listing(root: &Path, name: &str) -> Vec<String> {
    fs::read_to_string(root.join(format!("{name}.txt")))
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}
fn index(entry: &str) -> usize {
    entry
        .rsplit('_')
        .next()
        .unwrap()
        .split('.')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn split_is_scene_based_and_matches_legacy_seed_zero_selection() {
    let fixture = Fixture::new();
    fixture.session("sessionA", 100, &[], false);
    let summary = dataset::prepare(&fixture.root, &Options::default()).unwrap();
    assert_eq!((summary.train, summary.val), (80, 20));
    let train = listing(&fixture.root, "train");
    let val = listing(&fixture.root, "val");
    let train_scenes = train.iter().map(|e| index(e) / 10).collect::<BTreeSet<_>>();
    let val_scenes = val.iter().map(|e| index(e) / 10).collect::<BTreeSet<_>>();
    assert!(train_scenes.is_disjoint(&val_scenes));
    assert_eq!(val_scenes, [7, 8].into_iter().collect());
    assert_eq!(train.iter().chain(&val).collect::<BTreeSet<_>>().len(), 100);
    assert!(fs::read_to_string(fixture.root.join("data.yaml"))
        .unwrap()
        .contains("train: train.txt"));
}

#[test]
fn negative_samples_literal_newlines_and_orphans_are_preserved() {
    let fixture = Fixture::new();
    let session = fixture.session("sessionA", 100, &[3, 47, 88], false);
    fs::write(session.join("images/orphan.png"), b"fake").unwrap();
    let summary = dataset::prepare(&fixture.root, &Options::default()).unwrap();
    assert_eq!(summary.negatives, 3);
    for i in [3, 47, 88] {
        assert_eq!(
            fs::read_to_string(
                session
                    .join("labels")
                    .join(format!("sessionA_run_{i:06}.txt"))
            )
            .unwrap(),
            ""
        );
    }
    assert!(!session.join("labels/orphan.txt").exists());
    assert_eq!(
        fs::read_to_string(session.join("labels/sessionA_run_000000.txt")).unwrap(),
        "0 0.500000 0.500000 0.100000 0.100000\n"
    );
}

#[test]
fn insufficient_scenes_and_invalid_options_publish_no_labels_or_splits() {
    let fixture = Fixture::new();
    let session = fixture.session("sessionA", 8, &[], false);
    assert!(dataset::prepare(&fixture.root, &Options::default())
        .unwrap_err()
        .contains("cannot split"));
    assert!(!session.join("labels").exists());
    assert!(!fixture.root.join("val_fixed.txt").exists());
    for options in [
        Options {
            val_ratio: 0.,
            ..Default::default()
        },
        Options {
            val_ratio: 1.,
            ..Default::default()
        },
        Options {
            val_ratio: f64::NAN,
            ..Default::default()
        },
        Options {
            scene_size: 0,
            ..Default::default()
        },
    ] {
        assert!(dataset::prepare(&fixture.root, &options).is_err());
    }
}

#[test]
fn bad_label_anywhere_prevents_partial_publication() {
    for body in [
        "0 0.5 0.5 0.1\n",
        "0 x 0.5 0.1 0.1\n",
        "0 1.5 0.5 0.1 0.1\n",
        "-1 0.5 0.5 0.1 0.1",
        "0 NaN 0.5 0.1 0.1",
        "0 0.5 inf 0.1 0.1",
    ] {
        let fixture = Fixture::new();
        let session = fixture.session("sessionA", 20, &[], false);
        fs::write(session.join("annotations/sessionA_run_000019.txt"), body).unwrap();
        assert!(dataset::prepare(
            &fixture.root,
            &Options {
                scene_size: 5,
                ..Default::default()
            }
        )
        .unwrap_err()
        .contains("broken label"));
        assert!(!session.join("labels").exists());
        assert!(!fixture.root.join("train.txt").exists());
    }
}

#[test]
fn frozen_validation_is_stable_while_changed_labels_refresh() {
    let fixture = Fixture::new();
    let session = fixture.session("sessionA", 20, &[], false);
    let options = Options {
        scene_size: 5,
        ..Default::default()
    };
    dataset::prepare(&fixture.root, &options).unwrap();
    let val = fs::read(fixture.root.join("val_fixed.txt")).unwrap();
    fs::write(
        session.join("annotations/sessionA_run_000000.txt"),
        "0 0.25 0.25 0.2 0.2\n",
    )
    .unwrap();
    dataset::prepare(&fixture.root, &options).unwrap();
    assert_eq!(fs::read(fixture.root.join("val_fixed.txt")).unwrap(), val);
    assert_eq!(
        fs::read_to_string(session.join("labels/sessionA_run_000000.txt")).unwrap(),
        "0 0.250000 0.250000 0.200000 0.200000\n"
    );
}

#[test]
fn new_sessions_only_extend_training_and_do_not_move_frozen_validation() {
    let fixture = Fixture::new();
    fixture.session("sessionA", 100, &[], false);
    dataset::prepare(&fixture.root, &Options::default()).unwrap();
    let val = listing(&fixture.root, "val_fixed");
    fixture.session("sessionB", 100, &[], false);
    let summary = dataset::prepare(
        &fixture.root,
        &Options {
            seed: 42,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((summary.train, summary.val), (180, 20));
    assert_eq!(listing(&fixture.root, "val_fixed"), val);
    assert!(
        listing(&fixture.root, "train")
            .iter()
            .filter(|e| e.contains("sessionB/"))
            .count()
            == 100
    );
}

#[test]
fn missing_frozen_pairs_or_modified_frozen_manifest_require_explicit_repair() {
    let fixture = Fixture::new();
    fixture.session("sessionA", 100, &[], false);
    dataset::prepare(&fixture.root, &Options::default()).unwrap();
    let original = fs::read(fixture.root.join("train.txt")).unwrap();
    let val = listing(&fixture.root, "val_fixed");
    let stem = Path::new(&val[0]).file_stem().unwrap();
    fs::remove_file(
        fixture
            .root
            .join("sessionA/annotations")
            .join(stem)
            .with_extension("txt"),
    )
    .unwrap();
    assert!(dataset::prepare(&fixture.root, &Options::default())
        .unwrap_err()
        .contains("missing"));
    assert_eq!(fs::read(fixture.root.join("train.txt")).unwrap(), original);
    fs::write(
        fixture.root.join("val_fixed.txt"),
        "./sessionA/images/sessionA_run_000000.png\n",
    )
    .unwrap();
    assert!(dataset::prepare(&fixture.root, &Options::default())
        .unwrap_err()
        .contains("refreeze"));
    dataset::prepare(
        &fixture.root,
        &Options {
            refreeze: true,
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn replaced_frozen_image_is_detected_by_checksum() {
    let fixture = Fixture::new();
    fixture.session("sessionA", 100, &[], false);
    dataset::prepare(&fixture.root, &Options::default()).unwrap();
    let val = listing(&fixture.root, "val_fixed");
    fs::write(
        fixture.root.join(val[0].trim_start_matches("./")),
        b"replaced screenshot",
    )
    .unwrap();
    assert!(dataset::prepare(&fixture.root, &Options::default())
        .unwrap_err()
        .contains("frozen validation image changed"));
}

#[test]
fn late_frames_cannot_leak_into_a_frozen_validation_scene() {
    let fixture = Fixture::new();
    let session = fixture.session("sessionA", 20, &[], false);
    fs::remove_file(session.join("images/sessionA_run_000012.png")).unwrap();
    fs::remove_file(session.join("annotations/sessionA_run_000012.txt")).unwrap();
    let options = Options {
        scene_size: 5,
        ..Default::default()
    };
    dataset::prepare(&fixture.root, &options).unwrap();
    let before = fs::read(fixture.root.join("val_fixed.txt")).unwrap();
    fs::write(
        session.join("images/sessionA_run_000012.png"),
        b"late frame",
    )
    .unwrap();
    fs::write(
        session.join("annotations/sessionA_run_000012.txt"),
        "0 0.5 0.5 0.1 0.1\n",
    )
    .unwrap();
    assert!(dataset::prepare(&fixture.root, &options)
        .unwrap_err()
        .contains("splits a scene"));
    assert_eq!(
        fs::read(fixture.root.join("val_fixed.txt")).unwrap(),
        before
    );
}

#[test]
fn legacy_frozen_listing_imports_without_resplitting() {
    let fixture = Fixture::new();
    fixture.session("sessionA", 100, &[], false);
    let val = (70..80)
        .map(|i| format!("./sessionA/images/sessionA_run_{i:06}.png"))
        .collect::<Vec<_>>();
    fs::write(
        fixture.root.join("val_fixed.txt"),
        format!("{}\r\n", val.join("\r\n")),
    )
    .unwrap();
    let before = fs::read(fixture.root.join("val_fixed.txt")).unwrap();
    let summary = dataset::prepare(&fixture.root, &Options::default()).unwrap();
    assert_eq!((summary.train, summary.val), (90, 10));
    assert_eq!(
        fs::read(fixture.root.join("val_fixed.txt")).unwrap(),
        before
    );
    assert!(fixture.root.join("splits_manifest.json").is_file());
}

#[test]
fn coco_uses_existing_lists_and_keeps_empty_images_and_exact_pixel_boxes() {
    let fixture = Fixture::new();
    fixture.session("日本語", 20, &[3, 13], true);
    dataset::prepare(
        &fixture.root,
        &Options {
            scene_size: 5,
            ..Default::default()
        },
    )
    .unwrap();
    let train_before = fs::read(fixture.root.join("train.txt")).unwrap();
    let val_before = fs::read(fixture.root.join("val.txt")).unwrap();
    let outputs = dataset::coco(&fixture.root).unwrap();
    let mut images = 0;
    let mut boxes = 0;
    for path in outputs {
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        images += value["images"].as_array().unwrap().len();
        boxes += value["annotations"].as_array().unwrap().len();
        assert_eq!(
            value["categories"],
            serde_json::json!([{"id":1,"name":"chat","supercategory":"chat"}])
        );
        for item in value["annotations"].as_array().unwrap() {
            assert_eq!(item["bbox"], serde_json::json!([90.0, 45.0, 20.0, 10.0]));
            assert_eq!(item["area"], 200.0);
            assert_eq!(item["category_id"], 1);
        }
    }
    assert_eq!((images, boxes), (20, 18));
    assert_eq!(
        fs::read(fixture.root.join("train.txt")).unwrap(),
        train_before
    );
    assert_eq!(fs::read(fixture.root.join("val.txt")).unwrap(), val_before);
}

#[test]
fn coco_missing_images_labels_invalid_split_and_traversal_never_publish() {
    let fixture = Fixture::new();
    let session = fixture.session("sessionA", 20, &[], true);
    dataset::prepare(
        &fixture.root,
        &Options {
            scene_size: 5,
            ..Default::default()
        },
    )
    .unwrap();
    for relative in [
        "../outside.png",
        "C:/outside.png",
        "sessionA/images/../../outside.png",
        "sessionA/images/NUL.png",
    ] {
        fs::write(fixture.root.join("train.txt"), relative).unwrap();
        assert!(dataset::coco(&fixture.root).is_err());
        assert!(!fixture
            .root
            .join("annotations/instances_train_chatbox.json")
            .exists());
    }
    assert!(dataset::build_coco(&fixture.root, "../bad").is_err());
    fs::write(
        fixture.root.join("train.txt"),
        "./sessionA/images/sessionA_run_000000.png\n",
    )
    .unwrap();
    fs::remove_file(session.join("labels/sessionA_run_000000.txt")).unwrap();
    assert!(dataset::coco(&fixture.root)
        .unwrap_err()
        .contains("missing label"));
}

#[test]
fn standalone_dataset_cli_supports_prepare_and_coco() {
    let fixture = Fixture::new();
    fixture.session("日本語", 20, &[0], true);
    let binary = env!("CARGO_BIN_EXE_vrct-dataset");
    let prepare = std::process::Command::new(binary)
        .args(["prepare", "--root"])
        .arg(&fixture.root)
        .args(["--scene-size", "5", "--seed", "-42"])
        .output()
        .unwrap();
    assert!(
        prepare.status.success(),
        "{}",
        String::from_utf8_lossy(&prepare.stderr)
    );
    let coco = std::process::Command::new(binary)
        .args(["coco", "--root"])
        .arg(&fixture.root)
        .output()
        .unwrap();
    assert!(
        coco.status.success(),
        "{}",
        String::from_utf8_lossy(&coco.stderr)
    );
}

#[test]
fn coco_rejects_cross_split_image_leakage_before_publication() {
    let fixture = Fixture::new();
    fixture.session("sessionA", 20, &[], true);
    dataset::prepare(
        &fixture.root,
        &Options {
            scene_size: 5,
            ..Default::default()
        },
    )
    .unwrap();
    let val = fs::read(fixture.root.join("val.txt")).unwrap();
    fs::write(fixture.root.join("train.txt"), val).unwrap();
    assert!(dataset::coco(&fixture.root)
        .unwrap_err()
        .contains("both train and validation"));
    assert!(!fixture
        .root
        .join("annotations/instances_train_chatbox.json")
        .exists());
}

#[test]
fn explicit_refreeze_recovers_a_broken_split_manifest() {
    let fixture = Fixture::new();
    fixture.session("sessionA", 100, &[], false);
    dataset::prepare(&fixture.root, &Options::default()).unwrap();
    fs::write(fixture.root.join("splits_manifest.json"), b"broken JSON").unwrap();
    assert!(dataset::prepare(&fixture.root, &Options::default()).is_err());
    dataset::prepare(
        &fixture.root,
        &Options {
            refreeze: true,
            ..Default::default()
        },
    )
    .unwrap();
    let value: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("splits_manifest.json")).unwrap())
            .unwrap();
    assert_eq!(value["format_version"], 1);
    let mut incomplete = value;
    incomplete["frozen_images"] = serde_json::json!({});
    fs::write(
        fixture.root.join("splits_manifest.json"),
        serde_json::to_vec(&incomplete).unwrap(),
    )
    .unwrap();
    assert!(dataset::prepare(&fixture.root, &Options::default())
        .unwrap_err()
        .contains("checksum missing"));
    dataset::prepare(
        &fixture.root,
        &Options {
            refreeze: true,
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn coco_applies_exif_rotation_before_pixel_coordinates() {
    use image::ImageEncoder;
    let fixture = Fixture::new();
    let session = fixture.session("sessionA", 20, &[], true);
    let path = session.join("images/sessionA_run_000000.png");
    let mut encoder = image::codecs::png::PngEncoder::new(fs::File::create(path).unwrap());
    encoder
        .set_exif_metadata(vec![
            73, 73, 42, 0, 8, 0, 0, 0, 1, 0, 18, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ])
        .unwrap();
    encoder
        .write_image(
            &vec![0; 200 * 100 * 3],
            200,
            100,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    dataset::prepare(
        &fixture.root,
        &Options {
            scene_size: 5,
            ..Default::default()
        },
    )
    .unwrap();
    let train = dataset::build_coco(&fixture.root, "train").unwrap();
    let image = train["images"]
        .as_array()
        .unwrap()
        .iter()
        .find(|image| image["file_name"].as_str().unwrap().ends_with("000000.png"))
        .unwrap();
    assert_eq!(
        (image["width"].as_u64(), image["height"].as_u64()),
        (Some(100), Some(200))
    );
    let annotation = train["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["image_id"] == image["id"])
        .unwrap();
    assert_eq!(annotation["bbox"], serde_json::json!([45., 90., 10., 20.]));
}
