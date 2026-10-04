//! Native ONNX Runtime lookup for inference tests; no interpreter lookup.
use std::path::PathBuf;

pub fn library() -> PathBuf {
    if let Some(path) = std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from) {
        assert!(
            path.is_file(),
            "ORT_DYLIB_PATH is not a native runtime file: {}",
            path.display()
        );
        return path;
    }
    let name = if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    let mut candidates = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../resources/onnxruntime")
        .join(name)];
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(name));
            candidates.push(directory.join("resources/onnxruntime").join(name));
        }
    }
    if let Some(path) = candidates.iter().find(|path| path.is_file()) {
        return path.clone();
    }
    panic!("Native ONNX Runtime is required for this test. Set ORT_DYLIB_PATH to {name}, or run cargo run -p xtask -- prepare from src-tauri. Searched: {candidates:?}");
}
