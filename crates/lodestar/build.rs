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
    for var in [
        "LODESTAR_COMMIT",
        "LODESTAR_CHANNEL",
        "SOURCE_DATE_EPOCH",
        "LODESTAR_VERSION",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    // Only this checkout's own git, never an unrelated repository the
    // source tree happens to sit inside.
    let git = git_dirs(&root);
    if let Some((dir, common)) = &git {
        let watched = [
            dir.join("HEAD"),
            common.join("packed-refs"),
            common.join("refs/heads"),
            common.join("refs/tags"),
        ];
        for path in watched.iter().filter(|p| p.exists()) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    // A commit hash, or nothing: it's passed to git and embedded.
    let hash = |c: &String| (7..=40).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit());
    let given = var("LODESTAR_COMMIT").map(|c| c.trim().to_string());
    if let Some(c) = given.as_ref().filter(|c| !hash(c)) {
        println!(
            "cargo::warning=LODESTAR_COMMIT={c:?} isn't a 7 to 40 character hex hash: ignored"
        );
    }
    let commit = given.filter(hash).or_else(|| {
        git.as_ref()
            .and_then(|_| git_out(&root, &["rev-parse", "HEAD"]))
            .filter(hash)
    });
    let date = var("SOURCE_DATE_EPOCH").or_else(|| match (&commit, &git) {
        (Some(c), Some(_)) => git_out(&root, &["log", "-1", "--format=%ct", c]),
        _ => None,
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
    let version = build_version(var("LODESTAR_VERSION"));
    for (name, value) in [
        ("VERSION", Some(version)),
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

/// This build's version for the updater (§12.2, B-update): `$LODESTAR_VERSION`
/// when it's strict semver (a pre-release such as `-nightly.20261023`
/// allowed, no build metadata) whose `major.minor.patch` is the crate's,
/// else the crate's own `CARGO_PKG_VERSION`, with a warning.
fn build_version(given: Option<String>) -> String {
    let own = env!("CARGO_PKG_VERSION").to_string();
    let Some(v) = given.map(|v| v.trim().to_string()) else {
        return own;
    };
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v.as_str(), None),
    };
    let ident = |s: &str| {
        !s.is_empty()
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !(s.len() > 1 && s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit()))
    };
    let ok = core == own && pre.is_none_or(|p| p.split('.').all(ident)) && v.len() <= 64;
    if ok {
        v
    } else {
        println!(
            "cargo::warning=LODESTAR_VERSION={v:?} isn't strict semver on {own}: building as {own}"
        );
        own
    }
}

/// The checkout's git directory and its common directory (they differ in
/// a worktree, where `.git` is a file naming the git directory).
fn git_dirs(root: &Path) -> Option<(PathBuf, PathBuf)> {
    let dot = root.join(".git");
    let dir = if dot.is_dir() {
        dot
    } else {
        let text = std::fs::read_to_string(&dot).ok()?;
        let named = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
        if named.is_absolute() {
            named
        } else {
            root.join(named)
        }
    };
    let common = match std::fs::read_to_string(dir.join("commondir")) {
        Ok(text) => {
            let named = PathBuf::from(text.trim());
            if named.is_absolute() {
                named
            } else {
                dir.join(named)
            }
        }
        Err(_) => dir.clone(),
    };
    Some((dir, common))
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
