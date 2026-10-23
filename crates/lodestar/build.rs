//! Embeds the Svelte UI (`web/build/`, or `$LODESTAR_WEB_BUILD`) when the
//! `web` feature is on, precompressed and with its CSP computed; otherwise,
//! or when the UI hasn't been built, a stub page. See `src/web_build.rs`.
//! Also the build details `/system` reports (`build_info`).

use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
#[path = "src/web_build.rs"]
mod web_build;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/web_build.rs");
    println!("cargo:rerun-if-env-changed=LODESTAR_WEB_BUILD");
    let dir = std::env::var_os("CARGO_FEATURE_WEB").map(|_| {
        let dir = match std::env::var_os("LODESTAR_WEB_BUILD") {
            Some(d) => PathBuf::from(d),
            None => PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
                .join("../../web/build"),
        };
        // Cargo scans a directory recursively, and a missing path reruns
        // this script (and recompiles the crate) on every build. So until
        // the build exists, watch its parent, which sees it appear. That
        // scan includes node_modules: about 0.3 s on a no-op build, against
        // seconds for a recompile. Narrower existing paths (package.json)
        // don't change when `npm run build` runs, so they'd miss it.
        let watch = match (dir.exists(), dir.parent()) {
            (false, Some(parent)) if parent.exists() => parent,
            _ => &dir,
        };
        println!("cargo:rerun-if-changed={}", watch.display());
        dir
    });
    let bundle = web_build::bundle(dir.as_deref()).expect("bundling the web UI");
    if dir.is_some() && !bundle.built {
        println!(
            "cargo::warning=web/build not found: embedding the stub page (run `npm run build` in web/)"
        );
    }
    build_info(&bundle);
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let src = web_build::emit(&bundle, &out).expect("writing the embedded UI");
    std::fs::write(out.join("web_assets.rs"), src).expect("writing web_assets.rs");
}

/// The build details `GET /system` reports (`src/system/info.rs`), as
/// `LODESTAR_BUILD_*` environment variables for `env!`; empty means
/// unknown. Never the time of the build, so builds stay reproducible:
/// - the commit: `$LODESTAR_COMMIT`, else `git rev-parse HEAD`;
/// - the date: `$SOURCE_DATE_EPOCH`, else the commit's date;
/// - the channel: `$LODESTAR_CHANNEL` (`stable`, `beta`, `nightly`),
///   else `dev`;
/// - the target and `rustc -V`;
/// - the UI build id from the bundle's `_app/version.json`.
///
/// It reruns only when those variables or the checked-out commit change:
/// git's files are watched only if they exist, since a missing path would
/// rerun it on every build.
fn build_info(bundle: &web_build::Bundle) {
    for var in ["LODESTAR_COMMIT", "LODESTAR_CHANNEL", "SOURCE_DATE_EPOCH"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let git = root.join(".git");
    if git.is_dir() {
        for watched in ["HEAD", "packed-refs", "refs/heads", "refs/tags"] {
            let path = git.join(watched);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let commit = var("LODESTAR_COMMIT").or_else(|| git_out(&root, &["rev-parse", "HEAD"]));
    let date = var("SOURCE_DATE_EPOCH").or_else(|| match &commit {
        Some(c) => git_out(&root, &["log", "-1", "--format=%ct", c]),
        None => None,
    });
    let channel = match var("LODESTAR_CHANNEL").as_deref() {
        None => "dev".to_string(),
        Some(c @ ("stable" | "beta" | "nightly" | "dev")) => c.to_string(),
        Some(other) => {
            println!(
                "cargo::warning=LODESTAR_CHANNEL={other:?} isn't stable, beta, nightly or dev: building as dev"
            );
            "dev".to_string()
        }
    };
    let rustc = std::env::var_os("RUSTC")
        .and_then(|rustc| Command::new(rustc).arg("-V").output().ok())
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let ui = bundle
        .files
        .iter()
        .find(|f| f.path == "_app/version.json")
        .and_then(|f| ui_version(&f.raw));
    let date = date.filter(|d| d.trim().parse::<u64>().is_ok());
    for (name, value) in [
        ("COMMIT", commit),
        ("DATE", date),
        ("CHANNEL", Some(channel)),
        ("TARGET", var("TARGET")),
        ("RUSTC", rustc),
        ("UI", ui),
    ] {
        let value = value.unwrap_or_default().replace(['\n', '\r'], " ");
        println!("cargo:rustc-env=LODESTAR_BUILD_{name}={}", value.trim());
    }
}

/// A git command's trimmed output, if git ran and succeeded.
fn git_out(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// `{"version":"…"}`'s value, without a JSON parser in the build script.
fn ui_version(raw: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(raw).ok()?;
    let rest = text.split_once("\"version\"")?.1;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let value = &rest[..rest.find('"')?];
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        .then(|| value.to_string())
}
