//! Owner-only file and directory permissions, per platform.
//!
//! Unix uses mode bits (`0700` directory, `0600` file). Windows has no mode
//! bits: everything under `%APPDATA%` is nominally per-user, but an inherited
//! ACL can still grant other local accounts read access, so the protections
//! are rewritten to a single ACE for the current user.

use std::path::Path;

/// Make `path` readable, writable and executable only by its owner.
///
/// Failure is not fatal at the call sites: the secret is still written, and a
/// tighter-than-default ACL is defence in depth rather than the only barrier.
/// Callers report the error when it matters.
pub fn owner_only(path: &Path, is_dir: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if is_dir { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(windows)]
    {
        restrict_windows_acl(path, is_dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, is_dir);
        Ok(())
    }
}

#[cfg(windows)]
fn restrict_windows_acl(path: &Path, is_dir: bool) -> std::io::Result<()> {
    use std::process::Command;

    // `/inheritance:r` drops every inherited ACE (BUILTIN\Users and the like)
    // and `/grant:r` leaves a single full-control entry for the current user.
    // The account is domain-qualified so a same-named local account on a
    // domain-joined machine is never the one granted access.
    let user = std::env::var("USERNAME").unwrap_or_default();
    if user.is_empty() {
        return Err(std::io::Error::other("USERNAME is not set"));
    }
    let account = match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => format!("{domain}\\{user}"),
        _ => user,
    };
    let mut command = Command::new("icacls");
    command.arg(path).arg("/inheritance:r");
    if is_dir {
        // Container/object inherit flags only mean something on a directory;
        // `/T` applies the same ACL to files that already exist inside it.
        command
            .arg("/grant:r")
            .arg(format!("{account}:(OI)(CI)F"))
            .arg("/T")
            .arg("/C");
    } else {
        command.arg("/grant:r").arg(format!("{account}:F"));
    }
    let status = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "icacls could not restrict the file to its owner",
        ))
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn owner_only_accepts_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, "{}").unwrap();
        owner_only(&path, false).unwrap();
        owner_only(dir.path(), true).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
