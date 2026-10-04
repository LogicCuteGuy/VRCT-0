# Native Rust development tools

The auxiliary collector, sample player, annotation, dataset conversion,
Whisper evaluation and detector training commands live in Rust crates under
`src-tauri/crates`. They do not spawn a Python interpreter.

Build from the repository with:

```powershell
npm run tools:build
npm run tools:package
```

The ZIP includes the eight executables below, editable multilingual sample
JSON, UTF-16-aware text/JSONL catalogues, native DLLs, resources, attribution,
build information and an integrity manifest. A SHA-256 sidecar accompanies the ZIP. Copy the
entire ZIP to another computer and extract it before running a tool.

| Executable | Purpose | Details |
|---|---|---|
| `vrct-chatbox-player` | OSC multilingual samples, list/dry-run and interactive playback | [Sample player](chatbox_sample_player.md) |
| `vrct-dataset-collector` | Fresh VRChat HWND/OpenVR capture, automatic/manual labeled collection | [Collection](ocr_dataset_collection.md) |
| `vrct-capture-probe` | Native capture diagnostics and optional screenshots | [Collection](ocr_dataset_collection.md) |
| `vrct-annotator` | Offline job preparation/status/export and explicit Gemini annotation | [Annotation](gemini_annotation_workflow.md) |
| `vrct-dataset` | Scene-level YOLO split and COCO conversion | [Dataset](native_dataset.md) |
| `vrct-whisper-prepare` | Reproducible Common Voice/WAV/noise evaluation sets | [Whisper](../tools/whisper_eval/README.md) |
| `vrct-transcription-eval` | Native VAD/transcription pipeline, CER and timing reports | [Whisper](../tools/whisper_eval/README.md) |
| `vrct-yolox` | Train Tiny/Nano detectors, export decoded ONNX, calibrate INT8, evaluate detection | [Training](ocr_yolo_training.md) |

Run each executable with `--help` for its complete arguments. Preparation,
export, dry-run and tests do not upload source images/audio. Annotation and
Google transcription commands send the selected data only when invoked.

The detector trainer uses native Candle automatic differentiation. CPU is
the default. A CUDA build requires the `vrct-yolox/cuda` feature and the CUDA
toolchain. Training checkpoints use safetensors plus explicit optimizer and
epoch metadata; authorized YOLOX pretrained `.pth` state dictionaries are
read as data by the Rust loader. The restricted upstream VRCT detector is
excluded from distribution.

## Validation boundaries

Native tool migration is under validation. Synthetic and localhost tests
exercise file integrity, option validation, pure image/audio processing and
protocols. Real hardware capture, paid Gemini calls, long detector training
and CUDA behavior require separate validation.
