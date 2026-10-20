//! Fetches one info dictionary from one peer and reports the outcome.
//!
//! ```sh
//! cargo run -p lodestar-peer-wire --example fetch_one -- 127.0.0.1:6881 <40-hex info hash> [prefer|require|disable]
//! ```

use std::time::Instant;

use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::{Encryption, FetchOptions, fetch};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: fetch_one <addr> <info hash> [prefer|require|disable]");
        std::process::exit(2);
    }
    let addr = args[1].parse().expect("socket address");
    let hash: InfoHash = args[2].parse().expect("40-hex info hash");
    let encryption = match args.get(3).map(String::as_str) {
        Some("require") => Encryption::Require,
        Some("disable") => Encryption::Disable,
        _ => Encryption::Prefer,
    };
    let opts = FetchOptions {
        encryption,
        ..FetchOptions::default()
    };
    let started = Instant::now();
    match fetch(addr, hash, &opts).await {
        Ok(got) => {
            let info = Info::parse(&got.info).expect("verified metadata parses");
            println!(
                "ok {} bytes in {:?}: name={:?} files={} v2_key={} encrypted={} client={:?} pex={}",
                got.info.len(),
                started.elapsed(),
                info.name.decode(),
                info.files.len(),
                got.v2,
                got.encrypted,
                got.client,
                got.pex.len()
            );
        }
        Err(e) => {
            println!("error after {:?}: {e} ({:?})", started.elapsed(), e.kind());
            std::process::exit(1);
        }
    }
}
