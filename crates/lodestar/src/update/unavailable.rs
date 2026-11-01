//! `/updates` where there's no update engine: a fixture (served offline,
//! never checking) and, until E-M6's helper, Windows. `GET` says why
//! (`unavailable`); every write is `409 updates_unavailable`.

use lodestar_api::ops::BoxFuture;
use lodestar_api::updates::{
    Capability, CheckState, CheckStatus, Policy, RollbackInfo, Running, UpdatesApi, UpdatesError,
    UpdatesStatus, UpdatesUnavailable,
};

#[derive(Debug, Clone)]
pub struct Unavailable {
    pub why: UpdatesUnavailable,
    pub version: String,
}

impl Unavailable {
    fn status(&self) -> UpdatesStatus {
        UpdatesStatus {
            unavailable: Some(self.why),
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
                reason: Some(self.why.code().to_string()),
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
                held_in_process: false,
                allow_reindex: false,
            },
            releases: vec![],
            pending: None,
            installing: None,
            rollback: RollbackInfo {
                available: false,
                reason: Some(self.why.code().to_string()),
                to_version: None,
                lost_torrents: None,
                restores_store: None,
            },
            notices: vec![],
            history: vec![],
        }
    }

    fn refused<T: Send + 'static>(&self) -> BoxFuture<'_, Result<T, UpdatesError>> {
        let why = self.why;
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

    fn install(
        &self,
        _version: String,
        _when: lodestar_api::updates::InstallWhen,
        _confirm_manual: bool,
    ) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
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

    fn rollback(&self) -> BoxFuture<'_, Result<UpdatesStatus, UpdatesError>> {
        self.refused()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows' answer (`start` off Unix): `unavailable: "windows_helper"`,
    /// nothing installable, and every write refused with that reason.
    #[tokio::test]
    async fn windows_answers_windows_helper() {
        let u = Unavailable {
            why: UpdatesUnavailable::WindowsHelper,
            version: "0.5.3".into(),
        };
        let json = serde_json::to_value(UpdatesApi::status(&u).await).unwrap();
        assert_eq!(json["unavailable"], "windows_helper");
        assert_eq!(json["capability"]["can_install"], false);
        assert_eq!(json["capability"]["reason"], "windows_helper");
        assert_eq!(json["running"]["version"], "0.5.3");
        assert_eq!(json["releases"], serde_json::json!([]));
        assert_eq!(
            u.check().await.unwrap_err(),
            UpdatesError::Unavailable(UpdatesUnavailable::WindowsHelper)
        );
        assert!(u.skip("0.5.4".into()).await.is_err());
        assert!(u.pause(7).await.is_err());
        assert!(u.clear_history().await.is_err());
        assert_eq!(
            u.rollback().await.unwrap_err(),
            UpdatesError::Unavailable(UpdatesUnavailable::WindowsHelper)
        );
        assert_eq!(json["rollback"]["available"], false);
        assert_eq!(json["rollback"]["reason"], "windows_helper");
    }

    #[test]
    fn every_reason_has_its_wire_name() {
        for why in [
            UpdatesUnavailable::Fixture,
            UpdatesUnavailable::Network,
            UpdatesUnavailable::WindowsHelper,
        ] {
            assert_eq!(serde_json::to_value(why).unwrap(), why.code());
        }
    }
}
