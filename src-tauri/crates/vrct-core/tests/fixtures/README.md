# Frozen compatibility contracts

The golden JSON/JSONL files and tiny-model/tokenizer fixtures here record the
historical Python implementation. Their provenance is commit
`16cb286c62f407f75c3501df522c4997554d8934`, which retains the original generators
and settings-table extraction sources in Git history. The checked-in fixtures
are the contract inputs for native Rust tests; running a Python generator is
not part of building or validating this project.

Keep existing fixture values, request shapes, bytes, ordering and expected
errors when checking compatibility. Synthetic tiny CT2 models test tokenizer
and inference parity, rather than meaningful translation quality. Real-model
tests additionally require the external weights/audio named in their test files;
their existing availability checks still apply. Historical Python descriptions
explain expected behavior and do not indicate a runtime dependency.

The settings tables extracted from historical model modules are checked by
`settings_system`, `settings`, `config_bridge_contract` and `translation_catalog`.
Device-name/selection rules are checked by `audio_devices`; its Windows WASAPI
smoke test observes the current machine, so a successful listing does not prove
that every historical Python installation would enumerate identical hardware.

## Native validation

Run from `src-tauri`:

```powershell
cargo run -p xtask -- prepare --profile debug
cargo test --workspace -j1 -- --test-threads=1
cargo test -p vrct-core -j1 --test settings --test settings_system --test config_bridge_contract --test translation_catalog --test audio_devices -- --test-threads=1
cargo test -p vrct-core --features ct2 -j1 --test translation_ct2 -- --test-threads=1
cargo test -p vrct-core -j1 --test audio_silero -- --test-threads=1
```

The inference tests resolve ONNX Runtime through `ORT_DYLIB_PATH`, the prepared
`src-tauri/resources/onnxruntime/` directory, or the current executable's directory
(including its `resources/onnxruntime/` subdirectory). A missing native runtime
fails these tests with a preparation hint; they never launch Python. An explicit
`ORT_DYLIB_PATH` must point to an existing native library. Windows uses
`onnxruntime.dll`; other supported hosts use their native `.so`/`.dylib` library.
The current resource-preparation manifest targets Windows x64.

`audio_host` includes a live Windows speaker-loopback test, and `audio_devices`
includes device enumeration. Select fixture-only test targets when live device
interaction is outside the validation scope. No fixture regeneration is needed.
