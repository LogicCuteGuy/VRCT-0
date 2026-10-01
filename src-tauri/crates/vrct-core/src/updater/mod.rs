//! Self-updater driven by GitHub Releases.
//!
//! No `tauri-plugin-updater` and no signed manifest: the release list is read
//! from the GitHub API, the installer is picked by platform, and its
//! `<installer>.sha256` sidecar asset is verified before anything is launched.

pub mod asset;
pub mod download;
pub mod install;
pub mod release;

use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;

pub use asset::{Arch, Edition, Os, Target};
pub use release::{Channel, ReleaseInfo, UpdateCheck};

use release::GhRelease;

const API_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CHECKSUM_ATTEMPTS: u32 = 3;
const DOWNLOAD_ATTEMPTS: u32 = 5;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("network error: {0}")]
    Network(String),
    #[error("invalid version: {0}")]
    Version(#[from] semver::Error),
    #[error("no matching release")]
    NoRelease,
    #[error("version is not supported by the updater")]
    UnsupportedVersion,
    #[error("release has no installer for this platform")]
    NoInstaller,
    #[error("this platform is not supported by the updater")]
    UnsupportedPlatform,
    #[error("a .sha256 asset is published but could not be read; aborting rather than skipping verification")]
    ChecksumUnavailable,
    #[error("SHA-256 mismatch (expected {expected}, got {actual})")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("downloaded file is too small ({0} bytes) to be an installer")]
    TooSmall(u64),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<reqwest::Error> for UpdateError {
    fn from(error: reqwest::Error) -> Self {
        UpdateError::Network(error.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct UpdateSource {
    pub owner: String,
    pub repo: String,
    /// `https://api.github.com` unless a test points it elsewhere.
    pub api_base: String,
}

impl UpdateSource {
    pub fn github(owner: &str, repo: &str) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            api_base: "https://api.github.com".into(),
        }
    }

    fn releases_url(&self) -> String {
        format!(
            "{}/repos/{}/{}/releases?per_page=100",
            self.api_base, self.owner, self.repo
        )
    }
}

/// An installer that is downloaded and verified, ready to launch.
#[derive(Debug, Clone)]
pub struct PreparedInstaller {
    pub path: PathBuf,
    pub version: Version,
    /// False only for old releases that never published a `.sha256` sidecar.
    pub checksum_verified: bool,
}

pub struct Updater {
    client: reqwest::Client,
    source: UpdateSource,
    current: Version,
    min_supported: Version,
}

impl Updater {
    pub fn new(
        source: UpdateSource,
        current_version: &str,
        min_supported: &str,
    ) -> Result<Self, UpdateError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("VRCT-updater/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(API_TIMEOUT)
            .build()?;
        Ok(Self {
            client,
            source,
            current: Version::parse(current_version.trim_start_matches(['v', 'V']))?,
            min_supported: Version::parse(min_supported)?,
        })
    }

    async fn fetch_releases(&self) -> Result<Vec<GhRelease>, UpdateError> {
        let response = self
            .client
            .get(self.source.releases_url())
            .header("Accept", "application/vnd.github+json")
            .timeout(API_TIMEOUT)
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }

    pub async fn check(&self, channel: Channel) -> Result<UpdateCheck, UpdateError> {
        let releases = self.fetch_releases().await?;
        Ok(release::check_update(
            &releases,
            &self.min_supported,
            &self.current,
            channel,
        ))
    }

    pub async fn list_available(&self) -> Result<Vec<ReleaseInfo>, UpdateError> {
        let releases = self.fetch_releases().await?;
        Ok(release::release_infos(&releases, &self.min_supported))
    }

    /// Download and verify the installer for `target_version` (or the newest
    /// release on `channel`) into `dest_dir`.
    pub async fn prepare_install(
        &self,
        target_version: Option<&str>,
        channel: Channel,
        edition: Edition,
        dest_dir: &Path,
        progress: impl FnMut(u64, Option<u64>),
    ) -> Result<PreparedInstaller, UpdateError> {
        let target = Target::current().ok_or(UpdateError::UnsupportedPlatform)?;
        self.prepare_install_for(target, target_version, channel, edition, dest_dir, progress)
            .await
    }

    pub async fn prepare_install_for(
        &self,
        target: Target,
        target_version: Option<&str>,
        channel: Channel,
        edition: Edition,
        dest_dir: &Path,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<PreparedInstaller, UpdateError> {
        let releases = self.fetch_releases().await?;
        let (release, version) = match target_version {
            Some(wanted) => {
                release::find_release(&releases, &self.min_supported, wanted)
                    .ok_or(UpdateError::UnsupportedVersion)?
            }
            None => release::latest_for_channel(&releases, &self.min_supported, channel)
                .ok_or(UpdateError::NoRelease)?,
        };
        let installer = asset::select_installer(release, target, edition)
            .ok_or(UpdateError::NoInstaller)?;

        let expected_sha256 = match asset::select_checksum_asset(release, installer) {
            Some(sidecar) => Some(self.fetch_checksum(&sidecar.browser_download_url).await?),
            None => None,
        };

        tokio::fs::create_dir_all(dest_dir).await?;
        // Keep only the final path component so a hostile asset name cannot
        // escape `dest_dir`.
        let file_name = Path::new(&installer.name)
            .file_name()
            .ok_or(UpdateError::NoInstaller)?;
        let dest = dest_dir.join(file_name);
        let total = (installer.size > 0).then_some(installer.size);

        self.download(
            &installer.browser_download_url,
            &dest,
            expected_sha256.as_deref(),
            |downloaded| progress(downloaded, total),
        )
        .await?;

        Ok(PreparedInstaller {
            path: dest,
            version,
            checksum_verified: expected_sha256.is_some(),
        })
    }

    async fn fetch_checksum(&self, url: &str) -> Result<String, UpdateError> {
        for _ in 0..CHECKSUM_ATTEMPTS {
            let attempt = async {
                let text = self
                    .client
                    .get(url)
                    .timeout(API_TIMEOUT)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                Ok::<_, reqwest::Error>(asset::parse_sha256(&text))
            };
            if let Ok(Some(digest)) = attempt.await {
                return Ok(digest);
            }
        }
        Err(UpdateError::ChecksumUnavailable)
    }

    /// Network failures and truncated bodies are retried; a checksum mismatch
    /// is not, since re-fetching from the same source proves nothing.
    async fn download(
        &self,
        url: &str,
        dest: &Path,
        expected_sha256: Option<&str>,
        mut progress: impl FnMut(u64),
    ) -> Result<(), UpdateError> {
        let mut last_error = UpdateError::Network("no attempt made".into());
        for attempt in 0..DOWNLOAD_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
            }
            let response = match self.client.get(url).send().await {
                Ok(response) => response,
                Err(error) => {
                    last_error = error.into();
                    continue;
                }
            };
            let response = match response.error_for_status() {
                Ok(response) => response,
                Err(error) => {
                    last_error = error.into();
                    continue;
                }
            };
            let outcome = download::write_verified(
                Box::pin(response.bytes_stream()),
                dest,
                expected_sha256,
                download::MIN_INSTALLER_SIZE,
                &mut progress,
            )
            .await;
            match outcome {
                Ok(()) => return Ok(()),
                Err(error @ (UpdateError::Network(_) | UpdateError::TooSmall(_))) => {
                    last_error = error;
                }
                Err(fatal) => return Err(fatal),
            }
        }
        Err(last_error)
    }
}

#[cfg(test)]
mod tests;
