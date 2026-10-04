# Native dataset preparation

`vrct-dataset` prepares reviewed YOLO labels and converts the existing train/validation lists to COCO. It runs in Rust and does not launch Python. Gemini proposals must be reviewed in Label Studio before becoming training annotations.

```powershell
cargo build --manifest-path src-tauri/Cargo.toml -p vrct-annotator --release
src-tauri/target/release/vrct-dataset.exe prepare --root D:/dataset_annotated --val-ratio 0.2 --scene-size 10 --seed 0
src-tauri/target/release/vrct-dataset.exe coco --root D:/dataset_annotated
```

`prepare` also accepts the alias `yolo`, and `coco` accepts `yolox`. Without `--root`, both use `dataset_annotated` under the current directory.

## Input and output

Each input session contains `images/` and human-reviewed `annotations/`. Images may be PNG, JPG, or JPEG, paired with same-stem `.txt` annotations. Preparation writes normalized labels into each session's `labels/`, plus root-level `train.txt`, `val.txt`, `val_fixed.txt`, `splits_manifest.json`, and `data.yaml`. COCO writes `annotations/instances_train_chatbox.json` and `annotations/instances_val_chatbox.json` at the dataset root.

A YOLO row must contain exactly five fields: a nonnegative integer class ID and finite normalized center-x, center-y, width, height in 0..1. Coordinates are serialized to six decimal places. Literal backslash-n delimiters from older exports are repaired. A reviewed empty annotation remains an empty label and contributes a negative image. Missing annotation pairs are reported and skipped. A malformed annotation fails preparation before any labels or split files are published.

## Scenes and frozen validation

A filename ending in an integer frame index belongs to the scene formed from its run prefix and `index / scene-size`. Other filenames each form their own scene. Preparation shuffles scenes separately within each session using Python-compatible MT19937 signed 64-bit integer seeds, including negative seeds. It selects whole scenes for validation, keeping adjacent frames together. Sessions with insufficient independent scenes or no usable training/validation split fail before writes.

After the first preparation, `val_fixed.txt` is preserved byte for byte. Later sessions go into training, and edits to human annotations refresh the normalized labels. Existing frozen lists from the previous tools can be imported without an existing manifest. Use `--refreeze` only when intentionally changing the validation baseline.

`splits_manifest.json` records the frozen-list SHA256, split parameters, image and normalized-label checksums, scene identities, negative/positive state, and train/validation inventories. Changing a frozen image, removing its pair, or editing the frozen list fails validation. A newly discovered frame in a frozen validation scene also fails, preventing that scene from leaking into training. Restore the original data, use a new capture run, or explicitly refreeze. These checksum and late-frame checks strengthen the old frozen-list behavior.

## COCO conversion

Conversion uses the current train/validation lists without resampling or copying images. It reads actual image dimensions and applies image orientation, converts normalized center coordinates to pixel top-left xywh, calculates area, and preserves empty images. Duplicate images within a list or across the two splits are rejected. Both splits are validated and assembled before either COCO output is written.

The chat-box workflow uses YOLO class 0 and COCO category 1 named `chat`. As in the previous converter, category IDs are class IDs plus one; this command does not remap arbitrary classes or generate a multi-class category catalog.

Paths containing spaces and Unicode are supported. Dataset mutation uses an OS file lock and atomic file replacement. Traversal, absolute paths inside split lists, Windows device names, and linked paths outside the dataset are rejected.

## Regression checks

```powershell
cargo test --manifest-path src-tauri/Cargo.toml -p vrct-annotator
```

Tests cover deterministic scene splitting, negative samples, malformed-label rejection, fixed-list preservation, checksums, late-frame leakage, legacy frozen-list import, COCO pixel coordinates, missing annotations, traversal rejection, and both native CLIs. They use local synthetic images and do not train a model or call a paid API.
