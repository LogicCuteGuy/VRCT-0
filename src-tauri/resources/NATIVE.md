Native resource preparation is implemented by the Rust `xtask` crate:

```powershell
npm run native:prepare
npm run native:prepare-release
# Reuse only already verified source/cache files:
cargo run --manifest-path src-tauri/Cargo.toml -p xtask -- prepare --offline
```

The task verifies bundled Noto fonts and Valve's OpenVR client, downloads the
pinned Sudachi full dictionary and RapidOCR models when needed, and extracts
the CPU ONNX Runtime 1.26.0 x64 library and notices from Microsoft's official
archive. `native-manifest.json` pins that archive and its exact extracted files.
Existing verified resources are reused. CT2 and Whisper model weights are
downloaded by the app at run time and are not part of the application package.

The staged layout beside `VRCT.exe` is:

```
openvr_api.dll
onnxruntime.dll
onnxruntime_providers_shared.dll
msvcp140.dll
msvcp140_1.dll
vcruntime140.dll
vcruntime140_1.dll
resources/{transliteration,fonts,ocr}/...
resources/native-manifest.json
resources/msvc/manifest.json
licenses/...
```

`npm run dev` and `npm run build` enable the native CT2 feature. They never run
Python, construct a Python environment, or kill unrelated applications. The
legacy `dev-cuda`/`build-cuda` script names alias the CPU build; these aliases do
not advertise CUDA support. `npm run release` creates `VRCT-0.zip` with the root
`VRCT.exe` layout the custom NSIS installer expects.

The packager revalidates resource pins offline before creating a ZIP, includes
each payload's SHA-256 and size in `native-package-manifest.json`, and verifies
the written archive before publishing it. It rejects Python source/extensions,
Python runtime DLLs, the old sidecar, and the protected chat-bubble detector by
name or identical payload. Executables are also scanned for the complete model
embedded inside them, using a short fingerprint and the full object's SHA-256.
Failed packaging preserves the previous ZIP. The proprietary chat-bubble model
is excluded from fork releases; an explicitly supplied authorized external
model is required for chat-bubble recognition.

```powershell
npm run native:verify
```

The Windows runtime archive is from
[Microsoft's ONNX Runtime 1.26.0 release](https://github.com/microsoft/onnxruntime/releases/tag/v1.26.0).
The DLLs retain their MIT license and third-party notices in `licenses/`.

ONNX Runtime's required x64 VC++ Runtime DLLs are deployed beside the EXE.
Preparation discovers the installed Visual Studio C++ toolchain with vswhere
and copies only its release CRT files from VC/Redist/MSVC. It validates PE x64
DLL headers, records hashes and sizes in the generated MSVC manifest, and
includes the toolchain's Redist.txt and the Microsoft redistribution notice.
Verified cached files are reusable; absent files require that licensed build
toolchain. No DLLs are copied from System32 and no system installer is run.
