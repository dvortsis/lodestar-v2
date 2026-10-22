//! Embeds the Svelte UI (`web/build/`, or `$LODESTAR_WEB_BUILD`) when the
//! `web` feature is on, precompressed and with its CSP computed; otherwise,
//! or when the UI hasn't been built, a stub page. See `src/web_build.rs`.

use std::path::PathBuf;

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
        // Cargo scans a directory recursively. A missing path would rerun
        // this script on every build, so until the build exists, watch its
        // parent (which sees it appear).
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
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let src = web_build::emit(&bundle, &out).expect("writing the embedded UI");
    std::fs::write(out.join("web_assets.rs"), src).expect("writing web_assets.rs");
}
