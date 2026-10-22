//! Writes the API's OpenAPI document to `web/src/lib/api/openapi.json`
//! (or the path given), where `npm run api:types` turns it into the web
//! client's types. CI regenerates it and fails on any diff, so run this
//! after changing a DTO or a route and commit the result.
//!
//! `cargo run -p lodestar --bin openapi [-- <path>]`

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let path = std::env::args_os().nth(1).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/src/lib/api/openapi.json"),
        PathBuf::from,
    );
    match std::fs::write(&path, lodestar_api::openapi::json()) {
        Ok(()) => {
            let shown = path.canonicalize().unwrap_or(path);
            println!("wrote {}", shown.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("openapi: {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}
