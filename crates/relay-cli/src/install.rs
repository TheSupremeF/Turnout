//! `turnout install`: put this executable where the shell and the Agent can
//! find it, without an installer.
//!
//! A downloaded `turnout.exe` usually sits in Downloads, outside `PATH`, still
//! flagged as "from the internet". Installing the Agent from there is a trap:
//! the scheduled task records that path, so moving or deleting the download
//! later silently stops the Agent. This copies the binary to a per-user
//! location, adds it to the user `PATH` where that can be done safely, and
//! re-points an existing Agent at the new copy.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Per-user install directory: `%LOCALAPPDATA%\Turnout\bin` on Windows,
/// `~/.local/bin` elsewhere (already on `PATH` in most Linux shells).
pub fn install_dir() -> Result<PathBuf> {
    if cfg!(windows) {
        dirs::data_local_dir()
            .map(|base| base.join(relay_core::brand::APP_NAME).join("bin"))
            .ok_or_else(|| anyhow!("cannot locate %LOCALAPPDATA%"))
    } else {
        dirs::home_dir()
            .map(|home| home.join(".local").join("bin"))
            .ok_or_else(|| anyhow!("cannot locate the home directory"))
    }
}

fn binary_name() -> String {
    format!("{}{}", relay_core::brand::BIN_NAME, std::env::consts::EXE_SUFFIX)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether `dir` is one of the entries of `path_value` (case-insensitive and
/// trailing-separator-insensitive on Windows, where both vary freely).
fn path_contains(path_value: &str, dir: &Path) -> bool {
    let wanted = normalise(&dir.to_string_lossy());
    std::env::split_paths(path_value).any(|entry| normalise(&entry.to_string_lossy()) == wanted)
}

fn normalise(entry: &str) -> String {
    let trimmed = entry.trim().trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_string()
    }
}

/// Append `dir` to the user `PATH` on Windows. PowerShell's
/// `SetEnvironmentVariable(..., 'User')` writes HKCU\Environment and broadcasts
/// the change, so new terminals pick it up without signing out.
#[cfg(windows)]
fn add_to_user_path(dir: &Path) -> Result<bool> {
    let quoted = dir.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$d='{quoted}'; \
         $p=[Environment]::GetEnvironmentVariable('Path','User'); if (-not $p) {{ $p='' }}; \
         $have=($p -split ';' | ForEach-Object {{ $_.TrimEnd('\\') }}) -contains $d.TrimEnd('\\'); \
         if ($have) {{ exit 3 }}; \
         [Environment]::SetEnvironmentVariable('Path', (($p.TrimEnd(';') + ';' + $d).TrimStart(';')), 'User')"
    );
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .context("cannot run PowerShell to update PATH")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(3) => Ok(false),
        _ => Err(anyhow!("PowerShell could not update the user PATH")),
    }
}

/// Remove the "downloaded from the internet" mark, so SmartScreen does not
/// stop the installed copy (and the Agent started from it).
#[cfg(windows)]
fn clear_download_mark(file: &Path) {
    let mut stream = file.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let _ = std::fs::remove_file(PathBuf::from(stream));
}

pub struct Outcome {
    pub installed: PathBuf,
    /// The copy that was installed differs from the running one.
    pub copied: bool,
}

/// Copy the running executable into [`install_dir`] and make it reachable.
pub fn run() -> Result<Outcome> {
    let current = std::env::current_exe().context("cannot locate the Turnout executable")?;
    if current.to_string_lossy().contains(".app/Contents/") {
        anyhow::bail!(
            "this copy belongs to the Turnout desktop app; install the CLI from a release download"
        );
    }
    let dir = install_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let target = dir.join(binary_name());

    let copied = !same_file(&current, &target);
    if copied {
        // Copy to a staging name and rename, so an existing install (which may
        // be the running Agent's binary) is replaced in one step or not at all.
        let staged = dir.join(format!("{}.new", binary_name()));
        std::fs::copy(&current, &staged)
            .with_context(|| format!("cannot copy to {}", staged.display()))?;
        if target.exists() {
            // Windows refuses to overwrite a running .exe but allows renaming
            // it; `turnout update` cleans the `.old` file up on a later start.
            let mut retired = target.as_os_str().to_owned();
            retired.push(".old");
            let retired = PathBuf::from(retired);
            let _ = std::fs::remove_file(&retired);
            std::fs::rename(&target, &retired).with_context(|| {
                format!("cannot replace {} (is it in use?)", target.display())
            })?;
        }
        std::fs::rename(&staged, &target)
            .with_context(|| format!("cannot install {}", target.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    #[cfg(windows)]
    clear_download_mark(&target);

    println!("Installed {}", target.display());

    let on_path = std::env::var("PATH")
        .map(|value| path_contains(&value, &dir))
        .unwrap_or(false);
    #[cfg(windows)]
    {
        match add_to_user_path(&dir) {
            Ok(true) => println!(
                "Added {} to your PATH. Open a new terminal (and restart VS Code) to use `turnout`.",
                dir.display()
            ),
            Ok(false) if on_path => println!("{} is already on your PATH.", dir.display()),
            Ok(false) => println!(
                "{} is already on your PATH; open a new terminal to use `turnout`.",
                dir.display()
            ),
            Err(error) => println!(
                "Could not add {} to PATH ({error}). Add it by hand: Start → \
                 \"Edit environment variables for your account\" → Path → New.",
                dir.display()
            ),
        }
    }
    #[cfg(not(windows))]
    {
        if on_path {
            println!("{} is on your PATH.", dir.display());
        } else {
            // Shell startup files are the user's; say what to add rather than
            // editing them behind their back.
            println!(
                "{} is not on your PATH. Add this line to your shell profile \
                 (~/.zshrc or ~/.bashrc), then open a new terminal:\n\n  \
                 export PATH=\"{}:$PATH\"",
                dir.display(),
                dir.display()
            );
        }
    }

    Ok(Outcome { installed: target, copied })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_matching_ignores_trailing_separators() {
        let dir = Path::new("/home/u/.local/bin");
        let joined = std::env::join_paths(["/usr/bin", "/home/u/.local/bin/"]).unwrap();
        assert!(path_contains(&joined.to_string_lossy(), dir));
        let other = std::env::join_paths(["/usr/bin", "/home/u/.local/binx"]).unwrap();
        assert!(!path_contains(&other.to_string_lossy(), dir));
    }

    #[test]
    fn the_install_dir_is_per_user() {
        let dir = install_dir().unwrap();
        let home = dirs::home_dir().unwrap();
        assert!(dir.starts_with(&home), "{} is outside {}", dir.display(), home.display());
    }
}
