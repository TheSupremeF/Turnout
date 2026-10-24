// Embed the Windows icon into the CLI executable.
//
// Windows takes a program's icon from a resource section in the PE file, so the
// `.ico` has to be compiled in rather than shipped beside the binary. GNU's
// `windres` does that for the mingw target automatically when the resource is
// named `winresource.rc` and `windres` is on `PATH` — which is the case here,
// since mingw is needed to link that target at all.
//
// MSVC builds use `winres`, which is a separate dependency; that target is
// built in CI (see .github/workflows/release.yml), so it is handled there.

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

fn main() {
    println!("cargo:rerun-if-changed=assets/turnout.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env != "gnu" {
        // MSVC needs the `winres` crate; deliberately not a dependency of every
        // build just to set an icon.
        println!(
            "cargo:warning=not embedding the icon for the {target_env} Windows target"
        );
        return;
    }

    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let icon = root.join("assets").join("turnout.ico");
    if !icon.exists() {
        println!(
            "cargo:warning={} is missing; run `node scripts/generate-assets.mjs`",
            icon.display()
        );
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

    // Cargo runs build scripts with a PATH that does not always include
    // Homebrew or other package manager bins, so `windres` is looked up in the
    // usual install locations as well as on PATH.
    let windres = find_windres();
    let Some(windres) = windres else {
        println!(
            "cargo:warning=windres not found; the executable will have no icon \
             (install mingw-w64, or set WINDRES)"
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
        Ok(result) => println!(
            "cargo:warning=windres failed ({}); the executable will have no icon",
            String::from_utf8_lossy(&result.stderr).trim()
        ),
        Err(error) => println!(
            "cargo:warning=could not run {} ({error}); the executable will have no icon",
            windres.display()
        ),
    }
}
