//! The control channel between the child and the supervisor (§12.2
//! "must resolve"): reserved exit codes plus a run-bound request file.
//! The workspace forbids `unsafe`, so passing an extra descriptor isn't
//! an option.
//!
//! The child writes `request.json` with its own `run_id`, then exits with
//! the code after the graceful shutdown. A reserved code without a
//! matching request is an ordinary exit (or a failed attempt in
//! probation).

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::layout::Bin;

/// Restart the same version (B2c's restart, supervised). No attempt used.
pub const EXIT_RESTART: i32 = 80;
/// Switch to the staged overlay `request.version`.
pub const EXIT_SWITCH: i32 = 81;
/// Roll back to the previous version.
pub const EXIT_ROLLBACK: i32 = 82;

/// The environment variable the supervisor gives its child.
pub const ENV: &str = "LODESTAR_SUPERVISOR";
/// Its value is at most this long.
pub const MAX_ENV: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Restart,
    Switch,
    Rollback,
}

impl Action {
    pub fn code(self) -> i32 {
        match self {
            Action::Restart => EXIT_RESTART,
            Action::Switch => EXIT_SWITCH,
            Action::Rollback => EXIT_ROLLBACK,
        }
    }

    fn from_code(code: i32) -> Option<Action> {
        match code {
            EXIT_RESTART => Some(Action::Restart),
            EXIT_SWITCH => Some(Action::Switch),
            EXIT_ROLLBACK => Some(Action::Rollback),
            _ => None,
        }
    }
}

/// `request.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub run_id: String,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost_torrents: Option<u64>,
}

/// What the supervisor tells its child (`LODESTAR_SUPERVISOR`). Like B2c's
/// hints it isn't a setting, so it stays out of `config::ENV_VARS`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hint {
    pub pid: u32,
    pub run_id: String,
    /// The version the supervisor started (the overlay's, or its own).
    pub version: String,
    /// The supervisor's own (the image's) version.
    pub image_version: String,
    #[serde(default)]
    pub notices: Vec<String>,
}

impl Hint {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn decode(text: &str) -> Option<Hint> {
        if text.len() > MAX_ENV {
            return None;
        }
        serde_json::from_str(text).ok()
    }
}

/// 16 random hex digits, one per child start.
pub fn new_run_id() -> String {
    let mut bytes = [0u8; 8];
    // The OS RNG; a failure still yields a run id unique enough here.
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        bytes = (nanos ^ u64::from(std::process::id())).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The child's side: writes the request, then the code to exit with.
pub fn request(
    bin: &Bin,
    run_id: &str,
    action: Action,
    version: Option<String>,
    lost: Option<u64>,
) -> std::io::Result<i32> {
    bin.write(
        "request.json",
        &Request {
            run_id: run_id.to_string(),
            action,
            version,
            lost_torrents: lost,
        },
    )?;
    Ok(action.code())
}

/// The supervisor's side, after a child exited with `code`: the request
/// that goes with it, if the code is reserved and `request.json` names
/// this run. The file is consumed either way.
pub fn take(bin: &Bin, run_id: &str, code: Option<i32>) -> Option<Request> {
    let read = bin.read::<Request>("request.json").ok().flatten();
    let _ = bin.remove("request.json");
    let action = Action::from_code(code?)?;
    read.filter(|r| r.run_id == run_id && r.action == action)
}

/// What the updater (part 5) asks `serve` to do after its graceful
/// shutdown, instead of restarting: switch to a staged version, or roll
/// back. `main.rs` turns it into `request.json` and the exit code.
/// An action, the version it names, and the torrents a rollback loses.
pub type Intent = (Action, Option<String>, Option<u64>);

static INTENT: Mutex<Option<Intent>> = Mutex::new(None);

/// Records an intent; the caller then asks for the graceful shutdown
/// through `Restart::global().request()`.
pub fn set_intent(action: Action, version: Option<String>, lost: Option<u64>) {
    *INTENT.lock().unwrap_or_else(|e| e.into_inner()) = Some((action, version, lost));
}

pub fn intent() -> Option<Intent> {
    INTENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_counts_only_for_its_run_and_code() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let code = request(&bin, "aa", Action::Switch, Some("0.6.0".into()), None).unwrap();
        assert_eq!(code, EXIT_SWITCH);
        assert_eq!(
            take(&bin, "aa", Some(EXIT_SWITCH))
                .unwrap()
                .version
                .as_deref(),
            Some("0.6.0")
        );
        // Consumed: a second exit with the code is ordinary.
        assert!(take(&bin, "aa", Some(EXIT_SWITCH)).is_none());
        // Another run's request, or another code, doesn't count, and is
        // removed all the same.
        request(&bin, "bb", Action::Restart, None, None).unwrap();
        assert!(take(&bin, "aa", Some(EXIT_RESTART)).is_none());
        assert!(!bin.exists("request.json"));
        request(&bin, "aa", Action::Restart, None, None).unwrap();
        assert!(take(&bin, "aa", Some(EXIT_ROLLBACK)).is_none());
        request(&bin, "aa", Action::Restart, None, None).unwrap();
        assert!(take(&bin, "aa", None).is_none());
    }

    #[test]
    fn the_hint_round_trips_and_is_bounded() {
        let h = Hint {
            pid: 1,
            run_id: new_run_id(),
            version: "0.6.0".into(),
            image_version: "0.5.3".into(),
            notices: vec!["bad_overlay".into()],
        };
        assert_eq!(h.run_id.len(), 16);
        assert_eq!(Hint::decode(&h.encode()), Some(h));
        assert!(Hint::decode(&"x".repeat(MAX_ENV + 1)).is_none());
        assert!(Hint::decode("{}").is_none());
        // The codes stay clear of 0–3, clap's 2, a panic's 101 and 128 + n.
        for c in [EXIT_RESTART, EXIT_SWITCH, EXIT_ROLLBACK] {
            assert!((4..101).contains(&c));
        }
    }
}
