use super::release::{GhAsset, GhRelease};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub os: Os,
    pub arch: Arch,
}

impl Target {
    /// The platform this binary was compiled for.
    pub fn current() -> Option<Self> {
        let os = if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            return None;
        };
        let arch = if cfg!(target_arch = "x86_64") {
            Arch::X64
        } else if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else {
            return None;
        };
        Some(Self { os, arch })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    Cpu,
    Gpu,
}

impl Edition {
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("gpu") {
            Edition::Gpu
        } else {
            Edition::Cpu
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Edition::Cpu => "cpu",
            Edition::Gpu => "gpu",
        }
    }
}

const X64_TOKENS: [&str; 3] = ["x64", "x86_64", "amd64"];
const ARM64_TOKENS: [&str; 2] = ["arm64", "aarch64"];

fn is_installer_for(os: Os, lower_name: &str) -> bool {
    match os {
        Os::Windows => lower_name.ends_with("-setup.exe") || lower_name.ends_with(".msi"),
        Os::MacOs => lower_name.ends_with(".dmg"),
        Os::Linux => lower_name.ends_with(".appimage"),
    }
}

fn arch_matches(arch: Arch, lower_name: &str) -> bool {
    let (wanted, other) = match arch {
        Arch::X64 => (&X64_TOKENS[..], &ARM64_TOKENS[..]),
        Arch::Arm64 => (&ARM64_TOKENS[..], &X64_TOKENS[..]),
    };
    wanted.iter().any(|t| lower_name.contains(t)) && !other.iter().any(|t| lower_name.contains(t))
}

/// Pick the installer for `target` from a release.
///
/// Naming convention (matches Tauri's bundle names):
/// `VRCT_<ver>_x64-setup.exe`, `VRCT_<ver>_aarch64.dmg`, `VRCT_<ver>_amd64.AppImage`.
/// An asset containing `cuda` is the GPU edition. If a GPU edition is asked for
/// but the release has no CUDA asset, the standard one is used.
pub fn select_installer(release: &GhRelease, target: Target, edition: Edition) -> Option<&GhAsset> {
    let candidates: Vec<&GhAsset> = release
        .assets
        .iter()
        .filter(|asset| {
            let name = asset.name.to_ascii_lowercase();
            is_installer_for(target.os, &name) && arch_matches(target.arch, &name)
        })
        .collect();

    let is_cuda = |asset: &&GhAsset| asset.name.to_ascii_lowercase().contains("cuda");
    let wants_cuda = edition == Edition::Gpu;
    candidates
        .iter()
        .copied()
        .find(|asset| is_cuda(asset) == wants_cuda)
        .or_else(|| candidates.iter().copied().find(|asset| !is_cuda(asset)))
}

/// The `<installer>.sha256` sidecar published next to the installer.
pub fn select_checksum_asset<'a>(release: &'a GhRelease, installer: &GhAsset) -> Option<&'a GhAsset> {
    let wanted = format!("{}.sha256", installer.name);
    release.assets.iter().find(|asset| asset.name == wanted)
}

/// First whitespace-separated token if it is a 64-digit hex SHA-256. Accepts a
/// bare digest and `sha256sum` output (`<digest>  <file>`).
pub fn parse_sha256(text: &str) -> Option<String> {
    let digest = text.split_whitespace().next()?.to_ascii_lowercase();
    (digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())).then_some(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> GhAsset {
        GhAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.invalid/{name}"),
            size: 0,
        }
    }

    fn release_with(names: &[&str]) -> GhRelease {
        GhRelease {
            tag_name: "v1.0.0".into(),
            name: None,
            prerelease: false,
            draft: false,
            published_at: None,
            assets: names.iter().map(|n| asset(n)).collect(),
        }
    }

    const WIN: Target = Target { os: Os::Windows, arch: Arch::X64 };
    const MAC_ARM: Target = Target { os: Os::MacOs, arch: Arch::Arm64 };
    const LINUX: Target = Target { os: Os::Linux, arch: Arch::X64 };

    #[test]
    fn picks_the_matching_platform_installer() {
        let release = release_with(&[
            "VRCT_1.0.0_x64-setup.exe",
            "VRCT_1.0.0_x64-setup.exe.sha256",
            "VRCT_1.0.0_aarch64.dmg",
            "VRCT_1.0.0_x64.dmg",
            "VRCT_1.0.0_amd64.AppImage",
        ]);
        assert_eq!(select_installer(&release, WIN, Edition::Cpu).unwrap().name, "VRCT_1.0.0_x64-setup.exe");
        assert_eq!(select_installer(&release, MAC_ARM, Edition::Cpu).unwrap().name, "VRCT_1.0.0_aarch64.dmg");
        assert_eq!(select_installer(&release, LINUX, Edition::Cpu).unwrap().name, "VRCT_1.0.0_amd64.AppImage");
    }

    #[test]
    fn never_picks_a_checksum_or_wrong_arch() {
        let release = release_with(&["VRCT_1.0.0_x64-setup.exe.sha256", "VRCT_1.0.0_aarch64.dmg"]);
        assert!(select_installer(&release, WIN, Edition::Cpu).is_none());
        let intel_mac = Target { os: Os::MacOs, arch: Arch::X64 };
        assert!(select_installer(&release, intel_mac, Edition::Cpu).is_none());
    }

    #[test]
    fn gpu_edition_prefers_cuda_asset_and_falls_back_to_standard() {
        let both = release_with(&["VRCT_1.0.0_x64-setup.exe", "VRCT_cuda_1.0.0_x64-setup.exe"]);
        assert!(select_installer(&both, WIN, Edition::Gpu).unwrap().name.contains("cuda"));
        assert!(!select_installer(&both, WIN, Edition::Cpu).unwrap().name.contains("cuda"));

        let standard_only = release_with(&["VRCT_1.0.0_x64-setup.exe"]);
        assert_eq!(
            select_installer(&standard_only, WIN, Edition::Gpu).unwrap().name,
            "VRCT_1.0.0_x64-setup.exe"
        );
    }

    #[test]
    fn cpu_edition_never_picks_cuda_only_release() {
        let cuda_only = release_with(&["VRCT_cuda_1.0.0_x64-setup.exe"]);
        assert!(select_installer(&cuda_only, WIN, Edition::Cpu).is_none());
    }

    #[test]
    fn checksum_asset_is_installer_name_plus_sha256() {
        let release = release_with(&["a-setup.exe", "a-setup.exe.sha256", "b-setup.exe.sha256"]);
        let installer = asset("a-setup.exe");
        assert_eq!(select_checksum_asset(&release, &installer).unwrap().name, "a-setup.exe.sha256");
        assert!(select_checksum_asset(&release, &asset("c-setup.exe")).is_none());
    }

    #[test]
    fn parses_bare_and_sha256sum_formats() {
        let digest = "ab".repeat(32);
        assert_eq!(parse_sha256(&digest), Some(digest.clone()));
        assert_eq!(parse_sha256(&format!("{}  file.exe\n", digest.to_uppercase())), Some(digest));
        assert_eq!(parse_sha256(""), None);
        assert_eq!(parse_sha256("<html>404</html>"), None);
        assert_eq!(parse_sha256(&"ab".repeat(31)), None);
        assert_eq!(parse_sha256(&"zz".repeat(32)), None);
    }
}
