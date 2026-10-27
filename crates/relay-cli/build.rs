// Embed the Windows icon into the CLI executable.
//
// Windows takes a program's icon from a resource section in the PE file, so the
// `.ico` has to be compiled in rather than shipped beside the binary. GNU's
// `windres` does that for the mingw target automatically when the resource is
// named `winresource.rc` and `windres` is on `PATH` — which is the case here,
// since mingw is needed to link that target at all.
//
// MSVC compiles the same script with the Windows SDK's `rc.exe` into a `.res`
// file, which `link.exe` accepts as an input. No crate is needed for either.

use std::path::PathBuf;

/// Locate `windres` on PATH or in the directories cross toolchains install to.
fn find_windres() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("WINDRES") {
        return Some(PathBuf::from(explicit));
    }
    let mut directories: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    for fixed in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/mingw64/bin",
        "/ucrt64/bin",
        "C:\\msys64\\mingw64\\bin",
        "C:\\msys64\\ucrt64\\bin",
    ] {
        directories.push(PathBuf::from(fixed));
    }
    // Prefer the target-prefixed binary; a bare `windres` can be a host tool.
    let names = ["x86_64-w64-mingw32-windres", "windres"];
    for name in names {
        for directory in &directories {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Locate the Windows SDK resource compiler: `RC`, PATH, or the newest SDK
/// under `Windows Kits\10\bin` (not on PATH outside a developer prompt).
fn find_rc() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("RC") {
        return Some(PathBuf::from(explicit));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let kits = PathBuf::from(
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into()),
    )
    .join("Windows Kits")
    .join("10")
    .join("bin");
    let mut versions = std::fs::read_dir(&kits)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("x64").join("rc.exe").is_file())
        .collect::<Vec<_>>();
    // SDK directories are named by version (10.0.22621.0), so the last one
    // in version order is the newest.
    versions.sort_by_key(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                name.split('.')
                    .map(|part| part.parse::<u32>().unwrap_or(0))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    versions.pop().map(|path| path.join("x64").join("rc.exe"))
}

/// Compile the resource script with `rc.exe` and hand the `.res` to the linker.
fn embed_with_rc(resource: &std::path::Path, out_dir: &std::path::Path) {
    let Some(rc) = find_rc() else {
        no_icon("rc.exe not found; the executable will have no icon (install the Windows SDK, or set RC)");
        return;
    };
    let compiled = out_dir.join("turnout.res");
    let output = std::process::Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&compiled)
        .arg(resource)
        .output();
    match output {
        Ok(result) if result.status.success() => {
            println!("cargo:rustc-link-arg={}", compiled.display());
        }
        Ok(result) => no_icon(&format!(
            "rc.exe failed ({}); the executable will have no icon",
            String::from_utf8_lossy(&result.stdout).trim()
        )),
        Err(error) => no_icon(&format!(
            "could not run {} ({error}); the executable will have no icon",
            rc.display()
        )),
    }
}

/// Report that the icon could not be embedded. Release builds set
/// `TURNOUT_REQUIRE_ICON=1` so that fails the build instead of shipping an
/// executable without one.
fn no_icon(message: &str) {
    if std::env::var("TURNOUT_REQUIRE_ICON").as_deref() == Ok("1") {
        panic!("{message}");
    }
    println!("cargo:warning={message}");
}

fn main() {
    println!("cargo:rerun-if-env-changed=TURNOUT_REQUIRE_ICON");
    println!("cargo:rerun-if-changed=assets/turnout.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let icon = root.join("assets").join("turnout.ico");
    if !icon.exists() {
        no_icon(&format!(
            "{} is missing; run `node scripts/generate-assets.mjs`",
            icon.display()
        ));
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let resource = out_dir.join("turnout.rc");
    // windres resolves a relative icon path against its own working directory,
    // so the path is written quoted and absolute.
    let rc = format!(
        "1 ICON \"{}\"\n",
        icon.display().to_string().replace('\\', "\\\\")
    );
    std::fs::write(&resource, rc).expect("write the resource script");

    if target_env == "msvc" {
        embed_with_rc(&resource, &out_dir);
        return;
    }

    // Cargo runs build scripts with a PATH that does not always include
    // Homebrew or other package manager bins, so `windres` is looked up in the
    // usual install locations as well as on PATH.
    let windres = find_windres();
    let Some(windres) = windres else {
        no_icon(
            "windres not found; the executable will have no icon \
             (install mingw-w64, or set WINDRES)",
        );
        return;
    };
    let output = std::process::Command::new(&windres)
        .arg("--input")
        .arg(&resource)
        .arg("--output")
        .arg(out_dir.join("turnout.res.o"))
        .output();

    match output {
        Ok(result) if result.status.success() => {
            // `cargo:rustc-link-arg` is per-crate, and this crate is the binary.
            println!("cargo:rustc-link-arg={}", out_dir.join("turnout.res.o").display());
        }
        Ok(result) => no_icon(&format!(
            "windres failed ({}); the executable will have no icon",
            String::from_utf8_lossy(&result.stderr).trim()
        )),
        Err(error) => no_icon(&format!(
            "could not run {} ({error}); the executable will have no icon",
            windres.display()
        )),
    }
}
