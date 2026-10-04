# Native chat-bubble detector training

The dataset, trainer, ONNX exporter, INT8 calibrator and evaluator now have
Rust implementations. The trainer implements YOLOX Tiny/Nano with CSPDarknet,
PAN-FPN, decoupled heads and SimOTA matching. It uses Candle automatic
differentiation; no Python process or PyTorch installation is needed.

Only use weights and screenshots you are authorized to train with. The
restricted upstream VRCT detector is excluded from fork distributions and
must not be used for training, evaluation or distillation of another model.

## Build and prepare data

Install Rust, MSVC/Windows SDK and CMake. Build the tools:

```powershell
npm run tools:build
```

Executables are in `src-tauri/target/debug/`; for long training jobs use
`npm run tools:build-release` and `src-tauri/target/release/`.
[Native tool distribution](native_tools.md) describes ZIP packaging.

1. Capture screenshots with `vrct-dataset-collector`.
2. Prepare/annotate/export a job with `vrct-annotator`.
3. Review every image in Label Studio. Predictions are proposals, not verified
   labels. Keep verified empty images as negatives; never turn pending, failed
   or skipped annotations into negatives.
4. Place verified images and class-0 YOLO annotations in
   `dataset_annotated/<session>/images/` and `annotations/`.

```powershell
.\src-tauri\target\debug\vrct-dataset.exe prepare --root .\dataset_annotated --scene-size 10 --val-ratio 0.2 --seed 0
.\src-tauri\target\debug\vrct-dataset.exe coco --root .\dataset_annotated
```

See [dataset conversion](native_dataset.md) for frozen validation and the
exact CLI. Scene-level splitting prevents adjacent screenshots leaking into
both train and validation. After the first split, add data to train while
preserving `val_fixed.txt`; refreeze only deliberately.

## Train Tiny or Nano

Train from an authorized YOLOX COCO pretrained `.pth` checkpoint, or explicitly
select `--from-scratch`. The Rust `.pth` reader loads tensor data without
executing Python. Classification weights from the 80-class pretrained head
are replaced for the single `chat_box` class.

```powershell
.\src-tauri\target\release\vrct-yolox.exe train --root .\dataset_annotated --variant tiny --ckpt .\weights\yolox_tiny.pth --output .\runs\chatbox_yolox_tiny --epochs 80 --batch-size 8 --size 1280 --seed 0
.\src-tauri\target\release\vrct-yolox.exe train --root .\dataset_annotated --variant nano --ckpt .\weights\yolox_nano.pth --output .\runs\chatbox_yolox_nano --epochs 80 --batch-size 8 --size 1280 --seed 0
```

Tiny uses depth 0.33 / width 0.375. Nano uses depth 0.33 / width 0.25 and
depthwise convolutions. Training includes scene-preserving data selection,
30% mosaic, scale/translation/color augmentation, horizontal flips, multiscale
inputs, no-augmentation final epochs, warmup/cosine learning rate and SGD with
momentum/Nesterov. Mixup remains disabled.

The native trainer keeps float32 master parameters, uses EMA for evaluation
and exported checkpoints, and enables FP16 convolutions with dynamic loss
scaling for CUDA training (`--no-fp16` disables it). BatchNorm and losses remain
float32. Final no-augmentation epochs also enable box L1 loss. CPU training
uses float32; `--fp16 --device cpu` is rejected.
Its random number generator and color augmentation differ from PyTorch/YOLOX.
Revalidate accuracy on the frozen split rather than expecting identical numeric
results or checkpoint bytes from the historical trainer. History is explicit JSON.

Use `train --help` for learning-rate, warmup, no-augmentation, evaluation
interval, size-range and device options. CPU is the default. Build
`vrct-yolox` with its `cuda` feature and the native CUDA toolchain to use
`--device cuda`; requesting CUDA in a CPU-only build errors explicitly.

Training publishes immutable checkpoint directories containing
`weights.safetensors` (EMA), `training.safetensors` (float32 master parameters),
`momentum.safetensors` and `state.json` (including EMA and loss-scaling state). `last.json`
and `best.json` point to the complete published checkpoint. `history.json`
records loss and validation AP. Resume by passing the directory named in
`last.json`:

```powershell
.\src-tauri\target\release\vrct-yolox.exe train --root .\dataset_annotated --variant tiny --resume --ckpt .\runs\chatbox_yolox_tiny\last-epoch-20-step-1000 --output .\runs\chatbox_yolox_tiny --epochs 80 --batch-size 8 --size 1280 --seed 0
```

The checkpoint name above is an example; use the actual pointer. Keep variant,
size, seed and batch size consistent when resuming. Native RNG, augmentation
and arithmetic differ from the former PyTorch pipeline; old training runs
are not expected to be numerically identical.

## Export decoded ONNX

```powershell
.\src-tauri\target\release\vrct-yolox.exe export --variant tiny --ckpt .\runs\chatbox_yolox_tiny\best-epoch-80-step-4000 --output .\weights\ocr\chatbox_yolox_tiny.onnx --size 736,1280 --dynamic
```

The example checkpoint path must be replaced by `best.json`'s actual target.
The exporter writes opset 17, input `images` and output `[1,N,6]` containing
`cx,cy,w,h,objectness,class_score`. Grid/stride decoding is inside the graph;
thresholding and NMS remain adjustable outside it. Dynamic export derives
its grid from the runtime height/width. Dimensions must be multiples of 32.

An authorized external detector can be selected for the app with
`VRCT_OCR_BUBBLE_MODEL`. Do not commit training data or generated detector
weights to the repository.

## Calibrate INT8 and evaluate

```powershell
.\src-tauri\target\release\vrct-yolox.exe quantize --input .\weights\ocr\detector-fp32.onnx --output .\weights\ocr\detector-int8.onnx --root .\dataset_annotated --split train --every 4 --size 736,1280
.\src-tauri\target\release\vrct-yolox.exe eval --model .\weights\ocr\detector-int8.onnx --root .\dataset_annotated --split val --imgsz 1280 --conf 0.15 --iou 0.65 --match-iou 0.5 --coco-eval --output .\runs\detector-evaluation.json
```

Native calibration uses real image activations, folds inference BatchNorm,
uses per-channel signed INT8 weights and unsigned INT8 activations, and leaves
prediction heads/sigmoid/grid decoding as FLOAT. It validates and runs the
quantized graph before atomically publishing the result. Existing output is
preserved on failure. Input and output paths must differ.

Evaluation reports matched/missed/extra candidates, IoU, CPU mean/median latency
and per-session counts. The AP report uses one-class COCO-style 101-point
interpolation at IoU 0.50:0.95, with at most 100 detections per image. It does
not claim the full multi-class/crowd/area-stratified pycocotools report. For
model comparisons keep the same frozen split, image size and thresholds.
Check accuracy again after every training or calibration change; quantization
can alter weak candidates even when inference gets faster.

## Verification

```powershell
cargo test --manifest-path src-tauri/Cargo.toml -p vrct-yolox -- --test-threads=1
```

Tests use synthetic data to check actual backbone gradients and parameter
updates, checkpoint resume, negative samples, AP/NMS, dynamic ONNX parity and
calibration/inference. These checks do not establish real VRChat accuracy,
long-run convergence, CUDA behavior or hardware capture performance.
