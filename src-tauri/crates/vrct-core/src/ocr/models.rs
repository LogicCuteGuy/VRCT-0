use super::Config;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Deserialize)]
struct Manifest {
    models: Vec<Model>,
}
#[derive(Deserialize)]
struct Model {
    id: String,
    file: String,
    url: String,
    sha256: String,
}
const MANIFEST: &str = include_str!("../../../../resources/ocr/models.json");

pub fn resolve(config: &Config, id: &str, cancelled: Arc<AtomicBool>) -> Result<PathBuf, String> {
    let manifest: Manifest = serde_json::from_str(MANIFEST).map_err(|e| e.to_string())?;
    let model = manifest
        .models
        .into_iter()
        .find(|model| model.id == id)
        .ok_or_else(|| format!("Unknown OCR model {id}"))?;
    let download_root = config.local.join("weights/ocr");
    let mut roots = Vec::new();
    if let Some(root) = std::env::var_os("VRCT_OCR_MODEL_DIR") {
        roots.push(PathBuf::from(root));
    }
    roots.push(config.local.join("resources/ocr"));
    roots.push(download_root.clone());
    roots.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/ocr"));
    for root in roots {
        let path = root.join(&model.file);
        if valid(&path, &model.sha256) {
            return Ok(path);
        }
    }
    std::fs::create_dir_all(&download_root).map_err(|e| e.to_string())?;
    let path = download_root.join(&model.file);
    let temporary = download_root.join(format!("{}.{}.partial", model.file, std::process::id()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let result=runtime.block_on(async {
        let download=async {
            let client=reqwest::Client::builder().timeout(std::time::Duration::from_secs(180)).build().map_err(|e|e.to_string())?;
            let mut response=client.get(&model.url).send().await.map_err(|e|e.to_string())?.error_for_status().map_err(|e|e.to_string())?;
            use tokio::io::AsyncWriteExt;
            let mut file=tokio::fs::File::create(&temporary).await.map_err(|e|e.to_string())?;
            let mut length=0usize;
            let mut hash=Sha256::new();
            while let Some(chunk)=response.chunk().await.map_err(|e|e.to_string())? {
                length=length.checked_add(chunk.len()).ok_or("OCR download too large")?;
                if length>256*1024*1024 {return Err("OCR download exceeds 256 MiB".into());}
                hash.update(&chunk);
                file.write_all(&chunk).await.map_err(|e|e.to_string())?;
            }
            file.flush().await.map_err(|e|e.to_string())?;
            if hex::encode(hash.finalize())!=model.sha256 {return Err(format!("OCR model SHA256 mismatch: {}",model.file));}
            drop(file);
            if path.is_file() {tokio::fs::remove_file(&path).await.map_err(|e|e.to_string())?;}
            tokio::fs::rename(&temporary,&path).await.map_err(|e|e.to_string())?;
            Ok(())
        };
        let cancel=async {while !cancelled.load(Ordering::Acquire) {tokio::time::sleep(std::time::Duration::from_millis(50)).await;}};
        tokio::select! {result=download=>result,_=cancel=>Err("OCR model preparation cancelled".into())}
    });
    if temporary.is_file() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    Ok(path)
}
fn valid(path: &Path, expected: &str) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut hash = Sha256::new();
    let mut chunk = [0; 65536];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => hash.update(&chunk[..count]),
            Err(_) => return false,
        }
    }
    hex::encode(hash.finalize()) == expected
}
