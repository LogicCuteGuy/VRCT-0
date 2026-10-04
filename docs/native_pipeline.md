# Native Rust backend

The Windows application now starts the Rust controller directly. There is no
Python sidecar, opt-in environment switch, Python environment setup, or frozen
Python runtime in the application bundle. The React frontend still sends
base64 JSON requests and receives `{status, endpoint, result}` events.

The recorded public controller contract contains 304 endpoints. Application
construction refuses to start if any endpoint lacks a native owner.
Initialization builds its snapshot from the same native getters used after
startup; API-key getters return only the requested provider's key.

## Development and packaging

Install Node/npm, Rust, MSVC with the Windows SDK, and CMake. From the repository:

```powershell
npm install
npm run dev
npm run build
npm run release
```

These commands run the Rust `xtask` for version synchronization, resource
preparation, ZIP creation and verification. `package.json` is the version
authority. CTranslate2/Whisper are compiled into the application with the
`ct2` feature. This build offers CPU compute devices. The retained
`dev-cuda`/`build-cuda` aliases currently produce that same CPU build.

`native:prepare` downloads pinned and checksum-verified Sudachi, RapidOCR
and ONNX Runtime resources; it does not download multi-gigabyte translation
or speech weights. The native model manager downloads those on demand to
`weights/ctranslate2/<name>` and `weights/whisper/<name>`.
Downloads verify provider hashes, publish staged directories atomically,
report progress and support shutdown cancellation.

Preparation also stages the x64 Microsoft Visual C++ redistributable DLLs
from the installed MSVC toolchain, with their redistribution terms. These
app-local DLLs let ONNX Runtime load on computers without Visual Studio.

The release ZIP keeps `VRCT.exe`, native DLLs and licenses at its root, with
fonts, Sudachi and OCR resources under `resources/`. ZIP verification checks
entry lengths/hashes and rejects Python runtime files and the restricted
chat-bubble detector, including copies under another filename.

The frontend uses `useBackendRequest` for Tauri requests. Auxiliary capture,
annotation, dataset preparation, detector training/export/calibration and speech
evaluation are being verified as native Rust tools; see
[native tools](native_tools.md). Historical contract fixtures retain the
recorded outputs of the former backend for regression checks.

## Services and lifecycle

Settings are owned and atomically saved in Rust. Controller/authentication
handlers validate model choices, publish model catalogs, invalidate credentials
and refresh language/engine selections. Google, Bing and Papago web translation,
DeepL, hosted LLM providers and local HTTP providers use native HTTP clients.

Microphone/speaker sessions share one recorder per device for transcription
and volume meters. Construction and initialization open no capture stream.
Settings changes reconfigure active sessions; inactive sessions remain inactive.
The device monitor enumerates WASAPI devices without recording, preserves
unambiguous old MME names, follows defaults when requested and handles hotplug.

OSCQuery discovery, HTTP queries and UDP mute updates feed the microphone
lifecycle worker. OSC, WebSocket, OBS, clipboard and logging are native sinks.
WebSocket/OBS replacements reserve both sockets before publishing either, so a
failed bind leaves existing servers available. Clipboard paste requires a
successful copy and focus; absent SteamVR discovery permits copy only.

The overlay renderer uses bundled Noto fonts and native shaping. Clipboard,
overlay and mirror capture share a reference-counted OpenVR session. Overlay
startup retries when SteamVR is absent, without launching it. Rendering uses
Rust libraries, so antialiasing can differ from the former Pillow renderer.

OCR captures an OpenVR mirror or a configured HWND, recognizes text with
native ONNX sessions, and applies bounded deduplication before the message
pipeline. The original chat-bubble detector has a VRCT-only license:
development can use an authorized source copy; fork release packages exclude it.
An authorized external detector is required for bubble detection. See
[the original terms](licenses/chatbox/LICENSE.en.txt).
Recognition code and the general RapidOCR models do not require Python.

Telemetry preserves the existing enabled-only daily `app_started` and
`error_code` events and daily deduplication state. It does not send message
text/audio. Shutdown stops monitoring, recording, OCR, overlay, OSCQuery and
servers, cancels model/telemetry operations and flushes settings.

## Verification

Core tests use fake devices and scripted provider transports, compare the
recorded Python fixtures, audit all native routes and initialization getters,
and exercise live localhost sockets. Real ONNX recognition smoke tests and
native resource/ZIP verification are available independently of audio capture.
Hardware capture and real SteamVR/headset behavior require separate validation.

For a smoke launch without automatic large model downloads, set
`VRCT_SKIP_MODEL_DOWNLOAD=1`. Normal launches download missing selected
local weights in the background. Missing local weights do not disable cloud
engines; enabling an unavailable local engine reports an error.
