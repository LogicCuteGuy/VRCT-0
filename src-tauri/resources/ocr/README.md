# Native OCR model resources

`models.json` pins RapidOCR's PP-OCRv6 small multilingual detector/recognizer,
PP-OCRv5 mobile detector plus Korean, Cyrillic, Thai, Arabic and Devanagari
recognizers, and the PP-OCRv4 orientation classifier. All model checksums come
from the linked upstream manifest at its pinned Git commit. These RapidAI
models use Apache-2.0; the accompanying LICENSE is the upstream license.

Run `./prepare.ps1` before packaging to populate this folder with all models.
Rust also downloads a missing selected model into `PATH_LOCAL/weights/ocr`,
verifies SHA256 before loading, and uses no Python OCR package. Set
`VRCT_OCR_MODEL_DIR` to use an explicit prepared model directory.

The application-specific YOLOX bubble detector has a separate restrictive
license. It is never embedded or included in a fork's release. Authorized users
can provide `chatbox_yolox_tiny.onnx` in `PATH_LOCAL/weights/ocr` or set
`VRCT_OCR_BUBBLE_MODEL` to its file path. Enabling OCR without this file returns
`OCR_DISABLED_MODEL_MISSING`. Debug builds can use the existing source checkout's
model for local development. Its license and NOTICE remain in
`crates/vrct-core/assets/ocr/`.
