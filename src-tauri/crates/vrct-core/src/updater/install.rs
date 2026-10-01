use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::asset::Edition;
use super::release::Channel;

/// Values the Windows installer reads from its command line so its pages
/// start on what the user already has selected.
pub struct InstallOptions<'a> {
    pub edition: Edition,
    pub ui_language: &'a str,
    pub channel: Channel,
    /// Pin a specific version (set when the user picked one in the version picker).
    pub version: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The installer (or the replacement binary) is running; the app should quit.
    Launched,
    /// Nothing more can be automated; the user has to finish by hand at this path.
    Manual(PathBuf),
}

/// Launch an already verified installer for the current platform.
pub fn launch(installer: &Path, options: &InstallOptions<'_>) -> io::Result<InstallOutcome> {
    platform::launch(installer, options)
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    pub fn launch(installer: &Path, options: &InstallOptions<'_>) -> io::Result<InstallOutcome> {
        let mut command = Command::new(installer);
        command
            .arg(format!("/EDITION={}", options.edition.as_str()))
            .arg(format!("/UILANG={}", options.ui_language))
            .arg(format!("/CHANNEL={}", options.channel.as_str()));
        if let Some(version) = options.version {
            command.arg(format!("/VERSION={version}"));
        }
        if let Some(dir) = installer.parent() {
            command.current_dir(dir);
        }
        command
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()?;
        Ok(InstallOutcome::Launched)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    pub fn launch(installer: &Path, _options: &InstallOptions<'_>) -> io::Result<InstallOutcome> {
        // Only an AppImage can replace itself. Any other install (deb, dev
        // build) is left to the user, with the folder opened for them.
        let Some(current) = std::env::var_os("APPIMAGE").map(PathBuf::from) else {
            let folder = installer.parent().unwrap_or(installer);
            let _ = Command::new("xdg-open").arg(folder).spawn();
            return Ok(InstallOutcome::Manual(installer.to_path_buf()));
        };

        std::fs::set_permissions(installer, std::fs::Permissions::from_mode(0o755))?;
        replace_file(installer, &current)?;
        Command::new(&current).spawn()?;
        Ok(InstallOutcome::Launched)
    }

    /// Replacing the running file is fine on Linux (new inode), but the
    /// download directory may be on another filesystem, so fall back to a copy
    /// into the destination directory followed by an atomic rename.
    fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
        if std::fs::rename(from, to).is_ok() {
            return Ok(());
        }
        let mut staged = to.as_os_str().to_os_string();
        staged.push(".new");
        let staged = PathBuf::from(staged);
        std::fs::copy(from, &staged)?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&staged, to)?;
        let _ = std::fs::remove_file(from);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    pub fn launch(installer: &Path, _options: &InstallOptions<'_>) -> io::Result<InstallOutcome> {
        // Mounts the disk image; dragging the app into /Applications is the
        // user's step, so the running app must not quit underneath them.
        Command::new("open").arg(installer).spawn()?;
        Ok(InstallOutcome::Manual(installer.to_path_buf()))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    pub fn launch(installer: &Path, _options: &InstallOptions<'_>) -> io::Result<InstallOutcome> {
        Ok(InstallOutcome::Manual(installer.to_path_buf()))
    }
}
