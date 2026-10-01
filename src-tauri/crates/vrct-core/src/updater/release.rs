use semver::Version;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct GhAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

/// The subset of the GitHub "release" object the updater needs.
#[derive(Debug, Clone, Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

impl GhRelease {
    /// Tag `v3.5.1-beta.1` -> `3.5.1-beta.1`, falling back to the release name.
    pub fn version(&self) -> Option<Version> {
        let from_tag = self.tag_name.trim_start_matches(['v', 'V']);
        Version::parse(from_tag)
            .ok()
            .or_else(|| self.name.as_deref().and_then(|n| Version::parse(n).ok()))
    }
}

/// One row of the version picker, serialised like the Python `ReleaseInfo`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version: String,
    pub is_prerelease: bool,
    pub published_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
}

impl Channel {
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("beta") {
            Channel::Beta
        } else {
            Channel::Stable
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }
}

/// Result of an update check, same shape the UI already consumes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateCheck {
    pub is_update_available: bool,
    pub new_version: Option<String>,
}

/// Published, parseable, at least `min_supported`; newest first.
pub fn supported_releases<'a>(
    releases: &'a [GhRelease],
    min_supported: &Version,
) -> Vec<(&'a GhRelease, Version)> {
    let mut supported: Vec<_> = releases
        .iter()
        .filter(|release| !release.draft)
        .filter_map(|release| release.version().map(|version| (release, version)))
        .filter(|(_, version)| version >= min_supported)
        .collect();
    supported.sort_by(|a, b| b.1.cmp(&a.1));
    supported
}

/// Newest release on the channel. Beta users only see betas (and stable users
/// only stable), so a later stable release never shows up as a beta "update".
pub fn latest_for_channel<'a>(
    releases: &'a [GhRelease],
    min_supported: &Version,
    channel: Channel,
) -> Option<(&'a GhRelease, Version)> {
    supported_releases(releases, min_supported)
        .into_iter()
        .find(|(release, _)| release.prerelease == (channel == Channel::Beta))
}

pub fn find_release<'a>(
    releases: &'a [GhRelease],
    min_supported: &Version,
    version: &str,
) -> Option<(&'a GhRelease, Version)> {
    let wanted = Version::parse(version.trim_start_matches(['v', 'V'])).ok()?;
    supported_releases(releases, min_supported)
        .into_iter()
        .find(|(_, candidate)| *candidate == wanted)
}

pub fn check_update(
    releases: &[GhRelease],
    min_supported: &Version,
    current: &Version,
    channel: Channel,
) -> UpdateCheck {
    match latest_for_channel(releases, min_supported, channel) {
        Some((_, latest)) => UpdateCheck {
            is_update_available: latest > *current,
            new_version: Some(latest.to_string()),
        },
        None => UpdateCheck {
            is_update_available: false,
            new_version: None,
        },
    }
}

pub fn release_infos(releases: &[GhRelease], min_supported: &Version) -> Vec<ReleaseInfo> {
    supported_releases(releases, min_supported)
        .into_iter()
        .map(|(release, version)| ReleaseInfo {
            tag: release.tag_name.clone(),
            version: version.to_string(),
            is_prerelease: release.prerelease,
            published_at: release.published_at.clone().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn release(tag: &str, prerelease: bool, draft: bool) -> GhRelease {
        GhRelease {
            tag_name: tag.to_string(),
            name: None,
            prerelease,
            draft,
            published_at: Some("2026-01-01T00:00:00Z".to_string()),
            assets: vec![],
        }
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn sample() -> Vec<GhRelease> {
        vec![
            release("v3.6.0-beta.2", true, false),
            release("v3.5.2", false, false),
            release("v3.6.0-beta.1", true, false),
            release("v3.5.1", false, false),
            release("v3.9.9", false, true), // draft: must be ignored
            release("v3.4.2", false, false), // below minimum
            release("not-a-version", false, false),
        ]
    }

    #[test]
    fn version_comes_from_tag_then_name() {
        assert_eq!(release("v1.2.3", false, false).version(), Some(v("1.2.3")));
        let mut by_name = release("weird", false, false);
        by_name.name = Some("1.2.3".into());
        assert_eq!(by_name.version(), Some(v("1.2.3")));
        assert_eq!(release("weird", false, false).version(), None);
    }

    #[test]
    fn supported_filters_drafts_unparseable_and_old_then_sorts_newest_first() {
        let releases = sample();
        let tags: Vec<_> = supported_releases(&releases, &v("3.4.3"))
            .iter()
            .map(|(r, _)| r.tag_name.as_str())
            .collect();
        assert_eq!(tags, ["v3.6.0-beta.2", "v3.6.0-beta.1", "v3.5.2", "v3.5.1"]);
    }

    #[test]
    fn prerelease_sorts_below_its_release() {
        let releases = vec![
            release("v3.5.1-beta.1", true, false),
            release("v3.5.1", false, false),
        ];
        let supported = supported_releases(&releases, &v("3.4.3"));
        assert_eq!(supported[0].0.tag_name, "v3.5.1");
    }

    #[test]
    fn channel_selection_never_crosses_channels() {
        let releases = sample();
        let min = v("3.4.3");
        assert_eq!(
            latest_for_channel(&releases, &min, Channel::Stable).unwrap().1,
            v("3.5.2")
        );
        assert_eq!(
            latest_for_channel(&releases, &min, Channel::Beta).unwrap().1,
            v("3.6.0-beta.2")
        );
    }

    #[test]
    fn update_is_offered_only_when_strictly_newer() {
        let releases = sample();
        let min = v("3.4.3");
        let newer = check_update(&releases, &min, &v("3.5.1"), Channel::Stable);
        assert_eq!(
            newer,
            UpdateCheck {
                is_update_available: true,
                new_version: Some("3.5.2".into())
            }
        );
        let same = check_update(&releases, &min, &v("3.5.2"), Channel::Stable);
        assert!(!same.is_update_available);
        // A beta of the next minor is not an update for a stable user.
        let beta_user_on_latest = check_update(&releases, &min, &v("3.6.0-beta.2"), Channel::Beta);
        assert!(!beta_user_on_latest.is_update_available);
    }

    #[test]
    fn no_release_on_channel_means_no_update() {
        let releases = vec![release("v3.5.2", false, false)];
        let check = check_update(&releases, &v("3.4.3"), &v("3.5.1"), Channel::Beta);
        assert_eq!(
            check,
            UpdateCheck {
                is_update_available: false,
                new_version: None
            }
        );
    }

    #[test]
    fn find_release_accepts_tag_or_bare_version() {
        let releases = sample();
        let min = v("3.4.3");
        assert!(find_release(&releases, &min, "3.5.1").is_some());
        assert!(find_release(&releases, &min, "v3.5.1").is_some());
        assert!(find_release(&releases, &min, "3.4.2").is_none()); // below minimum
        assert!(find_release(&releases, &min, "3.9.9").is_none()); // draft
        assert!(find_release(&releases, &min, "nonsense").is_none());
    }

    #[test]
    fn release_infos_match_python_shape() {
        let infos = release_infos(&sample(), &v("3.4.3"));
        assert_eq!(
            infos[0],
            ReleaseInfo {
                tag: "v3.6.0-beta.2".into(),
                version: "3.6.0-beta.2".into(),
                is_prerelease: true,
                published_at: "2026-01-01T00:00:00Z".into(),
            }
        );
    }
}
