//! `/updates` where there's no update engine: a fixture (served offline,
//! never checking) and, until E-M6's helper, Windows. `GET` says why
//! (`unavailable`); every write is `409 updates_unavailable`.

use lodestar_api::ops::BoxFuture;
use lodestar_api::updates::{
    Capability, CheckState, CheckStatus, Policy, RollbackInfo, Running, UpdatesApi, UpdatesError,
    UpdatesStatus,
};

#[derive(Debug, Clone)]
pub struct Unavailable {
    pub why: &'static str,
    pub version: String,
}

impl Unavailable {
    fn status(&self) -> UpdatesStatus {
        UpdatesStatus {
            unavailable: Some(self.why.to_string()),
            status: CheckStatus {
                state: CheckState::Idle,
                checked_at: None,
                next_check_at: None,
                error: None,
            },
            running: Running {
                version: self.version.clone(),
                channel: "stable".into(),
                image_version: None,
                overlay: false,
                revoked_key: false,
                withdrawn: false,
            },
            capability: Capability {
                can_install: false,
                reason: Some(self.why.to_string()),
            },
            policy: Policy {
                mode: "off".into(),
                level: "fixes".into(),
                security_fixes: false,
                wait_days: 0,
                window: String::new(),
                window_zone: "UTC".into(),
                next_window_at: None,
                paused_until: None,
                allow_reindex: false,
            },
            releases: vec![],
            pending: None,
            rollback: RollbackInfo {
                available: false,
                to_version: None,
                lost_torrents: None,
            },
            notices: vec![],
            history: vec![],
        }
    }

    fn refused<T: Send + 'static>(&self) -> BoxFuture<'_, Result<T, UpdatesError>> {
        let why = self.why.to_string();
        Box::pin(async move { Err(UpdatesError::Unavailable(why)) })
    }
}

impl UpdatesApi for Unavailable {
    fn status(&self) -> BoxFuture<'_, UpdatesStatus> {
        let s = Unavailable::status(self);
        Box::pin(async move { s })
    }

    fn check(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }

    fn skip(&self, _version: String) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }

    fn pause(&self, _days: u32) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }

    fn clear_history(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }
}
