//! `lodestar companion list|install|remove|compose` (architecture §11;
//! plan 2026-11-06-v06-companions §4.3 CMP-5).
//!
//! - `list`: each catalogue companion, what's installed, and whether this
//!   machine can install it natively (or why not).
//! - `install <id>`: shows the pinned version and its SHA-256; installs
//!   only with `--sha256` naming exactly that hash. There's no `--yes`.
//! - `remove <id>`: shows exactly what it deletes; deletes with `--yes`.
//! - `compose <id>`: prints CMP-7's compose service.
//!
//! `install` and `remove` change the store's credential rows, so they run
//! with the server stopped, like `vacuum`.

use std::sync::{Arc, Mutex};

use clap::{Args, Subcommand};
use lodestar_companions::catalogue::Sha256;
use lodestar_companions::compose::{self, DataFolders, Paths, Route, ServiceInput};
use lodestar_companions::inputs::{ContainerName, HostPath, NetworkName, Pgid, Puid};
use lodestar_companions::manager::{InstallToken, Manager, ManagerError, Refusal};
use lodestar_companions::{Catalogue, CompanionId};
use lodestar_store::Store;

use super::lock::StoreLock;
use super::util::{Failure, Outcome, failed, refuse_during_update_rollback};
use crate::config::Config;

#[derive(Debug, Args, PartialEq, Eq)]
pub struct CompanionArgs {
    #[command(subcommand)]
    pub command: CompanionCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum CompanionCommand {
    /// Show each companion, what's installed and whether it can be
    /// installed here
    List,
    /// Install or update a companion (server stopped): without --sha256
    /// it shows what it would install
    Install {
        /// The companion's id, like qbittorrent
        id: String,
        /// The SHA-256 `install` showed, typed or pasted, to confirm
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
    },
    /// Remove an installed companion and its credentials (server
    /// stopped): without --yes it shows what it would delete
    Remove {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Print a compose service for running the companion in Docker
    Compose(ComposeArgs),
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct ComposeArgs {
    pub id: String,
    /// The host folder for its settings
    #[arg(long, value_name = "PATH")]
    pub config_dir: Option<String>,
    /// The host folder it saves downloads in
    #[arg(long, value_name = "PATH")]
    pub downloads: Option<String>,
    /// The Docker network Lodestar is on
    #[arg(long, default_value = "lodestar")]
    pub network: String,
    #[arg(long, default_value = "1000")]
    pub puid: String,
    #[arg(long, default_value = "1000")]
    pub pgid: String,
    /// Send its traffic through a gluetun service generated with it
    #[arg(long, conflicts_with = "gluetun_container")]
    pub gluetun: bool,
    /// Send its traffic through the gluetun container you already run
    #[arg(long, value_name = "NAME")]
    pub gluetun_container: Option<String>,
}

fn catalogue() -> Outcome<&'static Catalogue> {
    lodestar_companions::builtin().map_err(|e| {
        Failure::Failed(format!(
            "the companion catalogue is invalid: {}",
            e.rule().code()
        ))
    })
}

fn id(s: &str) -> Outcome<CompanionId> {
    CompanionId::parse(s)
        .map_err(|e| Failure::Refused(format!("{s:?} isn't a companion id: it is {e}")))
}

fn refused(e: ManagerError) -> Failure {
    let why = match &e {
        ManagerError::Refused(Refusal::InContainer) => {
            "Lodestar runs in a container, where companions aren't installed natively. Use \
             `lodestar companion compose` and run it as its own container."
        }
        ManagerError::Refused(Refusal::NotAvailable(_)) => {
            "this release has no pinned build of it for this machine"
        }
        ManagerError::Refused(Refusal::GuidedOnly) => {
            "on Windows its installer is offered for you to run (Settings → Companions)"
        }
        ManagerError::Refused(Refusal::Confirmation) => {
            "the SHA-256 doesn't match the pinned build; run `install` without --sha256 to see it"
        }
        ManagerError::Step {
            code: "not_wired", ..
        } => "this version can't start companions yet, so nothing was installed",
        _ => "",
    };
    let msg = if why.is_empty() {
        format!("{e}")
    } else {
        format!("{why} ({})", e.code())
    };
    match e {
        ManagerError::Refused(_) => Failure::Refused(msg),
        _ => Failure::Failed(msg),
    }
}

/// The manager over the store, with the server stopped.
fn open(config: &Config) -> Outcome<(Manager, tokio::runtime::Runtime)> {
    refuse_during_update_rollback(&config.data_dir)?;
    let db = config.db_path();
    // The store must be free (the server stopped); the companion lock
    // the manager takes then keeps serve's recovery out until we're done.
    StoreLock::acquire(&db)?.release()?;
    let store = Store::open(&db, config.store_config()).map_err(failed("opening the store"))?;
    let m = crate::companions::manager(
        config.data_dir.clone(),
        Arc::new(Mutex::new(store)),
        catalogue()?,
    )
    .map_err(|e| Failure::Failed(e.message))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(failed("starting the runtime"))?;
    // Whatever a crash interrupted is finished or undone first.
    // One companion's trouble doesn't stop the others' commands.
    for (c, r) in rt.block_on(m.recover()) {
        if let Err(e) = r {
            eprintln!(
                "{c}: couldn't finish or undo an interrupted change ({})",
                e.code()
            );
        }
    }
    Ok((m, rt))
}

pub fn run(config: &Config, args: CompanionArgs) -> Outcome {
    match args.command {
        CompanionCommand::List => list(config),
        CompanionCommand::Install { id: i, sha256 } => install(config, &id(&i)?, sha256.as_deref()),
        CompanionCommand::Remove { id: i, yes } => remove(config, &id(&i)?, yes),
        CompanionCommand::Compose(a) => {
            print!("{}", compose_text(&a)?);
            Ok(())
        }
    }
}

fn list(config: &Config) -> Outcome {
    let (m, _rt) = open(config)?;
    let installed = m.snapshot().borrow().companions.clone();
    for e in catalogue()?.entries() {
        let have = installed
            .iter()
            .find(|c| c.id == e.id)
            .and_then(|c| c.version.as_ref())
            .map_or("not installed".to_owned(), |v| format!("{v} installed"));
        let here = match m.offer(&e.id) {
            Ok(o) => format!("can install {}", o.version),
            Err(ManagerError::Refused(Refusal::Current)) => "up to date".to_owned(),
            Err(e) => format!("not installable here: {}", e.code()),
        };
        println!("{:<12} {:<16} {have}; {here}", e.id, e.name);
    }
    Ok(())
}

fn install(config: &Config, id: &CompanionId, sha256: Option<&str>) -> Outcome {
    let (m, rt) = open(config)?;
    let offer = m.offer(id).map_err(refused)?;
    let Some(typed) = sha256 else {
        println!(
            "{} {}{}\n  SHA-256 {}\n  {} bytes, licence {}\n\nTo install it, run:\n  lodestar companion install {} --sha256 {}",
            offer.name,
            offer.version,
            offer
                .replaces
                .as_ref()
                .map_or(String::new(), |v| format!(" (replaces {v})")),
            offer.sha256.to_hex(),
            offer.size,
            offer.licence,
            offer.id,
            offer.sha256.to_hex()
        );
        return Err(Failure::Refused(
            "not confirmed: nothing was changed".into(),
        ));
    };
    let sha = Sha256::parse_hex(typed.trim())
        .map_err(|_| Failure::Refused("--sha256 must be 64 hexadecimal digits".into()))?;
    let token = InstallToken {
        id: id.clone(),
        version: offer.version.clone(),
        sha256: sha,
    };
    let v = rt.block_on(m.install(&token)).map_err(refused)?;
    println!("{} {v} is installed.", offer.name);
    Ok(())
}

fn remove(config: &Config, id: &CompanionId, yes: bool) -> Outcome {
    let (m, rt) = open(config)?;
    let plan = m.removal(id).map_err(refused)?;
    let list = || {
        let mut s = format!(
            "  the folder {} and everything in it\n",
            plan.folder.display()
        );
        for r in &plan.rows {
            s.push_str(&format!("  the setting {r}, if it's there\n"));
        }
        s
    };
    if !yes {
        println!(
            "Removing {id} deletes:\n{}\nTo remove it, run:\n  lodestar companion remove {id} --yes",
            list()
        );
        return Err(Failure::Refused(
            "not confirmed: nothing was changed".into(),
        ));
    }
    rt.block_on(m.remove(id)).map_err(refused)?;
    println!("Removed {id}. Deleted:\n{}", list());
    Ok(())
}

fn compose_text(a: &ComposeArgs) -> Outcome<String> {
    fn bad(what: &'static str) -> impl Fn(lodestar_companions::inputs::InputError) -> Failure {
        move |e| Failure::Refused(format!("{what}: it is {e}"))
    }
    let path = |p: &Option<String>, what: &'static str| {
        p.as_deref()
            .map(HostPath::parse)
            .transpose()
            .map_err(bad(what))
    };
    let route = match (&a.gluetun_container, a.gluetun) {
        (Some(n), _) => {
            Route::ExistingGluetun(ContainerName::parse(n).map_err(bad("--gluetun-container"))?)
        }
        (None, true) => Route::Gluetun,
        (None, false) => Route::Direct,
    };
    let input = ServiceInput {
        puid: Puid::parse(&a.puid).map_err(bad("--puid"))?,
        pgid: Pgid::parse(&a.pgid).map_err(bad("--pgid"))?,
        network: NetworkName::parse(&a.network).map_err(bad("--network"))?,
        route,
        paths: Paths {
            config: path(&a.config_dir, "--config-dir")?,
            downloads: path(&a.downloads, "--downloads")?,
        },
        publish: Vec::new(),
        data: DataFolders::none(),
        taken: Vec::new(),
    };
    compose::compose(catalogue()?, &id(&a.id)?, &input).map_err(|e| Failure::Refused(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(id: &str) -> ComposeArgs {
        ComposeArgs {
            id: id.into(),
            config_dir: Some("/srv/qbittorrent".into()),
            downloads: Some("/srv/downloads".into()),
            network: "lodestar".into(),
            puid: "1000".into(),
            pgid: "1000".into(),
            gluetun: false,
            gluetun_container: None,
        }
    }

    #[test]
    fn compose_refuses_bad_input_and_unknown_companions() {
        let mut a = args("qbittorrent");
        a.puid = "x".into();
        assert!(matches!(compose_text(&a), Err(Failure::Refused(_))));
        assert!(matches!(
            compose_text(&args("Nope!")),
            Err(Failure::Refused(_))
        ));
        // The catalogue's images aren't pinned in this release, so nothing
        // is rendered: an image is never written without its digest.
        if lodestar_companions::builtin()
            .unwrap()
            .get("qbittorrent")
            .unwrap()
            .renderable()
            .is_err()
        {
            assert!(matches!(
                compose_text(&args("qbittorrent")),
                Err(Failure::Refused(_))
            ));
        }
    }
}
