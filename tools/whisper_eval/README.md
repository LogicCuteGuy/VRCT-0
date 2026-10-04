# Native Whisper evaluation tools

Dataset preparation and transcription evaluation now use Rust executables.
Python/NumPy are not required. Keep Common Voice audio outside Git and record
its release, source URL and license.

```powershell
cargo build --manifest-path src-tauri/Cargo.toml -p vrct-whisper-eval
src-tauri/target/debug/vrct-whisper-prepare.exe `
  --input-dir tmp/commonvoice-ja --tsv validated.tsv `
  --output-dir tmp/whisper_eval/dataset `
  --environment-noise tmp/noise/environment.wav `
  --count 150 --seed 20260911 --dataset-version "<common-voice-release>"
```

All preparation options remain: `--input-dir`, `--tsv`, `--output-dir`,
`--environment-noise`, `--count`, `--seed`, `--ffmpeg`, `--source-name`,
`--source-url`, `--license`, `--dataset-version`. CLI count remains 100-200;
the library permits smaller fixtures. Selection retains SHA-256 seed/ranks,
short/medium/long buckets, speaker round-robin and at least three speakers.
Positive and negative integer seeds are accepted.
Duplicate paths and empty transcripts are removed. Source/metadata paths are
checked against the corpus root, including symlink escapes. Conversion never
overwrites its source audio.

Output contains clean, white_snr10/20 and environment_snr10/20 WAV directories,
metadata.csv and manifest.json. These retain transcripts, speakers, source
paths/hashes, durations, source URL, release/license, seed and environment-noise
hash. Check total_clean_duration_seconds for a 20-30 minute evaluation; clip
count alone does not guarantee duration.

Audio is 16kHz mono signed16 little-endian PCM WAV, with conservative RMS/peak
normalization. Standard WAV inputs use native code; other formats/nonstandard
WAV require installed ffmpeg, optionally --ffmpeg <path>. Neither CLI downloads
corpora, models or decoders.

Noise uses ChaCha20-BoxMuller-v1, recorded in the manifest. It is repeatable in
the native implementation with the same SNR conditions, but newly generated
waveforms are **not byte-identical to NumPy PCG64 output**. Source selection and
seed hashes match Python. Previously prepared datasets remain valid inputs.

```powershell
src-tauri/target/debug/vrct-transcription-eval.exe `
  --repo-root . --dataset-dir tmp/whisper_eval/dataset `
  --output tmp/whisper_eval/result.json --engine Whisper `
  --condition clean --model base --compute-type int8 `
  --ort-library src-tauri/resources/onnxruntime/onnxruntime.dll
```

All evaluation options remain: --repo-root, --dataset-dir, --output, --condition,
--id, --engine, --model, --compute-type. Whisper weights must already exist under
weights/whisper/<model> beneath --repo-root. Missing weights are an explicit
error; no cloud fallback occurs. The evaluator selects the native CPU backend,
as the earlier evaluation harness did.
Builds enable VRCT's native CTranslate2 feature
and require its normal build prerequisites. ONNX Runtime is found through
--ort-library, ORT_DYLIB_PATH, beside the executable, or repository resources;
prepare it with npm run native:prepare.

Evaluation uses actual Silero VAD and VRCT phrase accumulation, silence padding,
recognition and transcript storage without opening a microphone. All VAD
segments are drained. Results include CER (Unicode NFKC, whitespace removal),
hypothesis/reference, confidence, queue status, segment reasons, attempts/
successes, pipeline errors, manifest hash, VAD/model-load/ASR timings, duration
and RTF. ASR RTF excludes model loading; pipeline RTF adds VAD. Model load time
is separate and recorded on the first successfully evaluated clip. Use --all for all matching rows
and --condition all for all five conditions; model weights are loaded once.
Batch evaluation stores failed clips alongside successful clips, with a failure
reason and null timings where measurements are unavailable. Single-clip setup
or VAD failures return an error. Exit codes are 0 for a passing evaluation,
1 for failed evaluation results and 2 for invalid input or setup errors.

--engine Google preserves the Google path and **sends the selected audio to
Google's speech endpoint**. Whisper stays local. Tests make no cloud calls.
Microphone/UI/controller checks and annotated VAD recall across at least two
real devices are separate from this benchmark.

```powershell
cargo test --manifest-path src-tauri/Cargo.toml -p vrct-whisper-eval
```

Tests include Python-derived selection/seed fixtures, Unicode CER, reproducible
SNR, strict WAV input, path containment, five-condition metadata/manifest,
native queue/storage success/no-match/failure, CLI validation and real Silero
inference on silence when the native runtime is prepared. Recognizer mocks are
used only in tests. Actual Whisper accuracy needs existing weights and suitable
licensed speech; synthetic tests do not establish it.
