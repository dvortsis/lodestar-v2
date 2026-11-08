//! Checks a `<channel>.json` envelope with the verifier clients run, for
//! the release workflow's round trip in CI (PK-manifest). It trusts only
//! the public keys it's given (a throwaway pair in CI, never the real
//! release key), starts from an empty trust state, and requires every
//! artifact of every release to resolve through `install`.
//!
//! ```text
//! cargo run -p lodestar-update --example verify_envelope -- \
//!     <envelope> <stable|beta|nightly> <channel-key.pub> <recovery.pub> <now>
//! ```

use std::process::ExitCode;

use lodestar_update::{Channel, Embedded, PublicKey, TrustState, check, install};

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("verify_envelope: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let [envelope, channel, key, recovery, now] = &args[..] else {
        return Err("usage: <envelope> <channel> <channel-key.pub> <recovery.pub> <now>".into());
    };
    let read = |p: &str| std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"));
    let public =
        |p: &str| PublicKey::from_file(&read(p)?).map_err(|e| format!("{p}: {}", e.code()));
    let channel = Channel::ALL
        .into_iter()
        .find(|c| c.name() == channel)
        .ok_or_else(|| format!("unknown channel {channel}"))?;
    let key = public(key)?;
    let embedded = Embedded {
        recovery: public(recovery)?,
        release: (channel != Channel::Nightly).then_some(key),
        nightly: (channel == Channel::Nightly).then_some(key),
        freshness: None,
        statements: Vec::new(),
    };
    let now: u64 = now.parse().map_err(|_| format!("bad time {now}"))?;
    let bytes = std::fs::read(envelope).map_err(|e| format!("{envelope}: {e}"))?;
    let state = TrustState::rebuild(&embedded, &[]).map_err(|e| e.code().to_string())?;
    let checked = check(&bytes, channel, now, &embedded, &state)
        .map_err(|e| format!("refused: {}", e.code()))?;
    if !checked.current {
        return Err("the manifest isn't current".into());
    }
    for release in &checked.manifest.releases {
        for a in &release.artifacts {
            let pin = install(&checked, &release.version, a.target, a.kind)
                .map_err(|e| format!("{} {:?}: {}", release.version, a.kind, e.code()))?;
            println!(
                "{} {:?} {:?} {} {}",
                pin.version, a.target, a.kind, pin.size, pin.url
            );
        }
    }
    println!(
        "accepted: {} manifest, {} releases, timestamp {}",
        channel.name(),
        checked.manifest.releases.len(),
        checked.manifest.timestamp
    );
    Ok(())
}
