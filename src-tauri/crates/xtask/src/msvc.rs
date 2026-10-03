//! App-local deployment from the build toolchain's licensed redistributables.
use super::{
    copy_file, file_checksum, publish_file, random, read_json, PackageFile, Result, Temporary,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const DLLS: [&str; 4] = [
    "msvcp140.dll",
    "msvcp140_1.dll",
    "vcruntime140.dll",
    "vcruntime140_1.dll",
];

#[derive(Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub format: u32,
    pub architecture: String,
    pub runtime_version: String,
    pub source: String,
    pub files: BTreeMap<String, PackageFile>,
    pub redist: PackageFile,
    pub notice: PackageFile,
}

pub(crate) fn validate_dll(bytes: &[u8]) -> Result<()> {
    let word = |offset| {
        bytes
            .get(offset..offset + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let pe = bytes
        .get(60..64)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
        .ok_or("MSVC runtime is not a valid PE DLL")?;
    if bytes.get(..2) != Some(b"MZ")
        || bytes.get(pe..pe.saturating_add(4)) != Some(b"PE\0\0")
        || word(pe.saturating_add(4)) != Some(0x8664)
        || word(pe.saturating_add(22)).is_none_or(|flags| flags & 0x2000 == 0)
        || word(pe.saturating_add(24)) != Some(0x20b)
    {
        return Err("MSVC runtime must be a release x64 PE32+ DLL".into());
    }
    Ok(())
}

pub(crate) fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.format != 1
        || manifest.architecture != "x64"
        || manifest.source != "Visual Studio VC/Redist/MSVC"
        || version_key(&manifest.runtime_version).is_none()
        || manifest.files.len() != DLLS.len()
        || DLLS.iter().any(|name| !manifest.files.contains_key(*name))
    {
        return Err("invalid MSVC redistribution manifest".into());
    }
    Ok(())
}

fn cached(directory: &Path) -> Result<Manifest> {
    let manifest: Manifest = read_json(&directory.join("manifest.json"))?;
    validate_manifest(&manifest)?;
    for name in DLLS {
        let path = directory.join(name);
        if file_checksum(&path)? != manifest.files[name] {
            return Err(format!("MSVC runtime checksum mismatch: {name}"));
        }
        validate_dll(&fs::read(path).map_err(|e| e.to_string())?)?;
    }
    for (name, expected) in [
        ("Redist.txt", &manifest.redist),
        ("NOTICE.md", &manifest.notice),
    ] {
        if file_checksum(&directory.join(name))? != *expected {
            return Err(format!("MSVC license checksum mismatch: {name}"));
        }
    }
    Ok(manifest)
}

fn version_key(version: &str) -> Option<Vec<u32>> {
    let parts: Vec<u32> = version
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    (parts.len() >= 3 && parts[0] >= 14).then_some(parts)
}

fn crt_directory(installation: &Path) -> Result<(String, PathBuf)> {
    let redist = installation.join("VC/Redist/MSVC");
    let mut versions = fs::read_dir(&redist)
        .map_err(|e| format!("Visual Studio CRT redistributables are missing: {e}"))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let version = entry.file_name().to_str()?.to_owned();
            Some((version_key(&version)?, version, entry.path()))
        })
        .collect::<Vec<_>>();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, version, path) in versions {
        let Ok(folders) = fs::read_dir(path.join("x64")) else {
            continue;
        };
        for folder in folders.filter_map(|entry| entry.ok()) {
            let name = folder.file_name().to_string_lossy().into_owned();
            if name.starts_with("Microsoft.VC")
                && name.ends_with(".CRT")
                && DLLS.iter().all(|dll| folder.path().join(dll).is_file())
            {
                return Ok((version, folder.path()));
            }
        }
    }
    Err("Visual Studio release x64 CRT redistributables are absent; install the MSVC C++ build tools with redistributables".into())
}

/// Use only release CRTs beneath a licensed Visual Studio installation.
/// Exposed for fixture validation; normal preparation discovers VS with vswhere.
pub fn stage_from_visual_studio(root: &Path, installation: &Path) -> Result<()> {
    let (runtime_version, source) = crt_directory(installation)?;
    let redist_path = installation.join("Licenses/1033/Redist.txt");
    let redist = file_checksum(&redist_path)
        .map_err(|e| format!("Visual Studio redistribution notice is missing: {e}"))?;
    let directory = root.join("src-tauri/resources/msvc");
    let notice = file_checksum(&directory.join("NOTICE.md"))?;
    let mut files = BTreeMap::new();
    // Validate the entire source set before replacing any cached resource.
    for name in DLLS {
        let path = source.join(name);
        validate_dll(&fs::read(&path).map_err(|e| e.to_string())?)?;
        files.insert(name.to_owned(), file_checksum(&path)?);
    }
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    for name in DLLS {
        let temporary = Temporary(directory.join(format!("{name}.{}.partial", random()?)));
        copy_file(&source.join(name), &temporary.0)?;
        if file_checksum(&temporary.0)? != files[name] {
            return Err("MSVC runtime changed while staging".into());
        }
        publish_file(&temporary.0, &directory.join(name))?;
    }
    copy_file(&redist_path, &directory.join("Redist.txt"))?;
    let manifest = Manifest {
        format: 1,
        architecture: "x64".into(),
        runtime_version,
        source: "Visual Studio VC/Redist/MSVC".into(),
        files,
        redist,
        notice,
    };
    let temporary = Temporary(directory.join(format!("manifest.{}.partial", random()?)));
    fs::write(
        &temporary.0,
        serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    publish_file(&temporary.0, &directory.join("manifest.json"))?;
    cached(&directory)?;
    Ok(())
}

fn discover_visual_studio() -> Result<PathBuf> {
    let programs = std::env::var_os("ProgramFiles(x86)")
        .ok_or("vswhere unavailable; install the Visual Studio MSVC C++ toolchain")?;
    let mut command = std::process::Command::new(
        PathBuf::from(programs).join("Microsoft Visual Studio/Installer/vswhere.exe"),
    );
    command.args([
        "-utf8",
        "-latest",
        "-products",
        "*",
        "-requires",
        "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
        "-property",
        "installationPath",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .map_err(|e| format!("cannot discover MSVC redistributables with vswhere: {e}"))?;
    let path = String::from_utf8(output.stdout)
        .map_err(|_| "vswhere returned invalid installation path")?;
    if !output.status.success() || path.trim().is_empty() {
        return Err("no installed Visual Studio MSVC C++ redistributables were found".into());
    }
    Ok(PathBuf::from(path.trim()))
}

pub(crate) fn prepare(root: &Path, output: &Path) -> Result<()> {
    let directory = root.join("src-tauri/resources/msvc");
    if cached(&directory).is_err() {
        stage_from_visual_studio(root, &discover_visual_studio()?)?;
    }
    cached(&directory)?;
    for name in DLLS {
        copy_file(&directory.join(name), &output.join(name))?;
    }
    for (source, target) in [
        ("Redist.txt", "licenses/MSVC-Redist.txt"),
        ("NOTICE.md", "licenses/MSVC-NOTICE.md"),
        ("manifest.json", "resources/msvc/manifest.json"),
    ] {
        copy_file(&directory.join(source), &output.join(target))?;
    }
    Ok(())
}
