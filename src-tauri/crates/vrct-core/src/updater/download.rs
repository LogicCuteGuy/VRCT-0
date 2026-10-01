use std::fmt::Display;
use std::path::{Path, PathBuf};

use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::UpdateError;

/// Smallest plausible installer; guards against saving an HTML error page.
pub const MIN_INSTALLER_SIZE: u64 = 1024 * 1024;

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Stream `stream` into `dest`, verifying size and (when given) SHA-256.
///
/// Bytes go to `<dest>.part` and are renamed only after verification, so a
/// partial or tampered file never exists at `dest`.
pub async fn write_verified<S, B, E>(
    mut stream: S,
    dest: &Path,
    expected_sha256: Option<&str>,
    min_size: u64,
    mut progress: impl FnMut(u64),
) -> Result<(), UpdateError>
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: Display,
{
    let part = part_path(dest);
    let result = write_to_part(&mut stream, &part, &mut progress).await;
    let (size, actual) = match result {
        Ok(done) => done,
        Err(error) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(error);
        }
    };

    let verdict = if size < min_size {
        Err(UpdateError::TooSmall(size))
    } else {
        match expected_sha256 {
            Some(expected) if !expected.eq_ignore_ascii_case(&actual) => {
                Err(UpdateError::ChecksumMismatch {
                    expected: expected.to_ascii_lowercase(),
                    actual,
                })
            }
            _ => Ok(()),
        }
    };
    if let Err(error) = verdict {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(error);
    }

    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

async fn write_to_part<S, B, E>(
    stream: &mut S,
    part: &Path,
    progress: &mut impl FnMut(u64),
) -> Result<(u64, String), UpdateError>
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: Display,
{
    let mut file = tokio::fs::File::create(part).await?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| UpdateError::Network(error.to_string()))?;
        let bytes = chunk.as_ref();
        file.write_all(bytes).await?;
        hasher.update(bytes);
        size += bytes.len() as u64;
        progress(size);
    }
    file.flush().await?;
    Ok((size, hex::encode(hasher.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn chunks(data: &[u8]) -> impl Stream<Item = Result<Vec<u8>, String>> + Unpin {
        stream::iter(
            data.chunks(7)
                .map(|c| Ok::<_, String>(c.to_vec()))
                .collect::<Vec<_>>(),
        )
    }

    fn sha(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vrct-core-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn good_download_is_renamed_into_place_and_reports_progress() {
        let dir = temp_dir("good");
        let dest = dir.join("setup.exe");
        let data = vec![9u8; 100];
        let mut last = 0;

        write_verified(chunks(&data), &dest, Some(&sha(&data)), 10, |n| last = n)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!part_path(&dest).exists());
        assert_eq!(last, 100);
    }

    #[tokio::test]
    async fn checksum_mismatch_leaves_nothing_behind() {
        let dir = temp_dir("mismatch");
        let dest = dir.join("setup.exe");
        let data = vec![1u8; 100];

        let error = write_verified(chunks(&data), &dest, Some(&"0".repeat(64)), 10, |_| {})
            .await
            .unwrap_err();

        assert!(matches!(error, UpdateError::ChecksumMismatch { .. }));
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[tokio::test]
    async fn too_small_download_is_rejected_even_without_a_checksum() {
        let dir = temp_dir("small");
        let dest = dir.join("setup.exe");

        let error = write_verified(chunks(b"<html>404</html>"), &dest, None, MIN_INSTALLER_SIZE, |_| {})
            .await
            .unwrap_err();

        assert!(matches!(error, UpdateError::TooSmall(16)));
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn stream_error_cleans_up_the_partial_file() {
        let dir = temp_dir("broken");
        let dest = dir.join("setup.exe");
        let broken = stream::iter(vec![Ok(vec![1u8; 50]), Err("connection reset".to_string())]);

        let error = write_verified(broken, &dest, None, 1, |_| {}).await.unwrap_err();

        assert!(matches!(error, UpdateError::Network(_)));
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[tokio::test]
    async fn digest_comparison_ignores_case() {
        let dir = temp_dir("case");
        let dest = dir.join("setup.exe");
        let data = vec![5u8; 64];
        write_verified(chunks(&data), &dest, Some(&sha(&data).to_uppercase()), 1, |_| {})
            .await
            .unwrap();
    }
}
