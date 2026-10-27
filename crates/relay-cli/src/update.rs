//! `turnout update`: replace this executable with the latest GitHub release.
//!
//! Each release carries one CLI file per platform, named
//! `turnout-<os>-<arch>[.exe]`, plus a `<name>.sha256` beside it. The download
//! is checked against that digest before anything on disk changes. Both files
//! come from the same release, so this guards against a truncated or corrupted
//! download, not against a compromised repository; releases are not signed.
//!
//! The running executable is renamed out of the way rather than overwritten:
//! Windows refuses to overwrite a running `.exe` but allows renaming it, and the
//! same sequence works on Unix. The renamed copy is removed on the next start.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Repository whose releases this build updates from.
pub const RELEASES_REPO: &str = "TheSupremeF/Turnout";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// The release file this build looks for.
fn asset_name() -> Result<String> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        other => bail!("no Turnout release is published for {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        other => bail!("no Turnout release is published for {other}"),
    };
    let suffix = if os == "windows" { ".exe" } else { "" };
    Ok(format!("turnout-{os}-{arch}{suffix}"))
}

/// `major.minor.patch` from a tag or version string; pre-release and build
/// suffixes are ignored, which is enough to order this project's releases.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        // GitHub's API rejects requests without a User-Agent.
        .user_agent(concat!("turnout/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("cannot create the HTTP client")
}

async fn latest_release(client: &reqwest::Client) -> Result<Release> {
    let url = format!("https://api.github.com/repos/{RELEASES_REPO}/releases/latest");
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("cannot reach GitHub")?;
    if !response.status().is_success() {
        bail!("GitHub answered {} for {url}", response.status());
    }
    response.json().await.context("unreadable release information")
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let response = client.get(url).send().await.with_context(|| format!("cannot download {url}"))?;
    if !response.status().is_success() {
        bail!("download failed with {} for {url}", response.status());
    }
    Ok(response.bytes().await?.to_vec())
}

/// Path the previous executable is moved to during an update.
pub(crate) fn retired_path(executable: &Path) -> PathBuf {
    let mut name = executable.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Remove the executable an earlier update moved aside. On Windows this fails
/// while that old process is still running; the next start tries again.
pub fn remove_retired_executable() {
    if let Ok(executable) = std::env::current_exe() {
        let _ = std::fs::remove_file(retired_path(&executable));
    }
}

/// Swap `executable` for `contents`, restoring the original on failure.
fn replace_executable(executable: &Path, contents: &[u8]) -> Result<()> {
    let retired = retired_path(executable);
    let staged = {
        let mut name = executable.as_os_str().to_owned();
        name.push(".new");
        PathBuf::from(name)
    };
    std::fs::write(&staged, contents)
        .with_context(|| format!("cannot write {}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = std::fs::remove_file(&retired);
    if let Err(error) = std::fs::rename(executable, &retired) {
        let _ = std::fs::remove_file(&staged);
        return Err(anyhow!(
            "cannot move {} aside ({error}); is it in a folder you can write to?",
            executable.display()
        ));
    }
    if let Err(error) = std::fs::rename(&staged, executable) {
        // Put the working binary back so the install is never left empty.
        let _ = std::fs::rename(&retired, executable);
        let _ = std::fs::remove_file(&staged);
        return Err(anyhow!("cannot install the new version ({error})"));
    }
    Ok(())
}

/// Check for, and unless `check_only`, install a newer release. Returns whether
/// the executable was replaced, so the caller can restart the agent.
pub async fn run(check_only: bool) -> Result<bool> {
    let current = env!("CARGO_PKG_VERSION");
    let executable = std::env::current_exe().context("cannot locate the Turnout executable")?;
    if executable.to_string_lossy().contains(".app/Contents/") {
        bail!("this copy belongs to the Turnout desktop app; update the app instead");
    }

    let client = client()?;
    let release = latest_release(&client).await?;
    let latest = parse_version(&release.tag_name)
        .ok_or_else(|| anyhow!("cannot read the version of release {}", release.tag_name))?;
    let installed = parse_version(current).expect("CARGO_PKG_VERSION is semver");
    if latest <= installed {
        println!("Turnout {current} is up to date (latest release: {}).", release.tag_name);
        return Ok(false);
    }
    println!("Turnout {} is available (installed: {current}).", release.tag_name);
    if check_only {
        println!("Run `turnout update` to install it, or see {}", release.html_url);
        return Ok(false);
    }

    let name = asset_name()?;
    let binary = release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| anyhow!("release {} has no {name}; see {}", release.tag_name, release.html_url))?;
    let digest_name = format!("{name}.sha256");
    let digest = release
        .assets
        .iter()
        .find(|asset| asset.name == digest_name)
        .ok_or_else(|| anyhow!("release {} has no {digest_name}; refusing an unverified download", release.tag_name))?;

    println!("Downloading {name}…");
    let contents = download(&client, &binary.browser_download_url).await?;
    let expected = String::from_utf8(download(&client, &digest.browser_download_url).await?)
        .context("the checksum file is not text")?;
    let expected = expected
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("the checksum file is empty"))?
        .to_ascii_lowercase();
    let actual = Sha256::digest(&contents)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected {
        bail!("checksum mismatch for {name} (expected {expected}, got {actual}); nothing was changed");
    }

    replace_executable(&executable, &contents)?;
    println!("Updated to {} at {}.", release.tag_name, executable.display());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_numerically_not_textually() {
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("0.2.0-beta.1"), Some((0, 2, 0)));
        assert_eq!(parse_version("v1"), Some((1, 0, 0)));
        assert_eq!(parse_version("nightly"), None);
    }

    #[test]
    fn replacing_keeps_the_old_binary_until_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("turnout");
        std::fs::write(&exe, b"old").unwrap();
        replace_executable(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(retired_path(&exe)).unwrap(), b"old");
        // A second update must not fail because the previous `.old` is there.
        replace_executable(&exe, b"newer").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"newer");
    }

    #[test]
    fn this_platform_has_a_release_file_name() {
        let name = asset_name().unwrap();
        assert!(name.starts_with("turnout-"), "{name}");
    }
}
