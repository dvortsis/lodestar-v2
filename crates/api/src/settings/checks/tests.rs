//! Each test kind against a local stand-in server, the SSRF guard's
//! refusals, and the saved-secret rule: a secret the form leaves out is
//! the saved one only while the address points where the saved one does.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

use axum::Router;
use axum::extract::{Query, RawQuery};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use lodestar_integrations::BoxFuture;
use serde_json::json;

use super::*;
use crate::settings::{
    Imported, Saved as SavedBatch, SectionValues, SettingsBatch, SettingsFailure, SettingsImport,
    SettingsIndex, SettingsPreview,
};

/// What a stand-in server received: each request's path and query, and
/// its body.
type Received = Arc<Mutex<Vec<(String, String)>>>;

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// A Torznab endpoint that wants `apikey=k1`.
async fn torznab_server(seen: Received) -> SocketAddr {
    serve(Router::new().route(
        "/api",
        get(move |RawQuery(q): RawQuery, Query(p): Query<Vec<(String, String)>>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push((q.unwrap_or_default(), String::new()));
                let key = p.iter().find(|(k, _)| k == "apikey").map(|(_, v)| v.as_str());
                if key == Some("k1") {
                    r#"<?xml version="1.0"?><caps><server version="1.4.2" title="Prowlarr"/></caps>"#
                } else {
                    r#"<error code="100" description="Incorrect user credentials"/>"#
                }
            }
        }),
    ))
    .await
}

/// Takes any POST and keeps it.
async fn sink(seen: Received) -> SocketAddr {
    serve(
        Router::new().fallback(post(move |headers: HeaderMap, body: String| {
            let seen = seen.clone();
            async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                seen.lock().unwrap().push((auth, body));
                // ntfy's answer; the webhook takes any 2xx.
                r#"{"id":"m1","event":"message"}"#
            }
        })),
    )
    .await
}

/// Points every name at one address.
struct Fixed(IpAddr);

impl Resolve for Fixed {
    fn resolve<'a>(&'a self, _: &'a str) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move { Ok(vec![self.0]) })
    }
}

fn to(ip: &str) -> Net {
    Net {
        resolver: Some(Arc::new(Fixed(ip.parse().unwrap()))),
        notify_policy: None,
    }
}

/// Reaches a stand-in on loopback, which notification channels may never
/// reach otherwise.
fn local() -> Net {
    Net {
        resolver: Some(Arc::new(Fixed([127, 0, 0, 1].into()))),
        notify_policy: Some(NetPolicy::download_client()),
    }
}

/// Section, key, the endpoints it's bound to, and the secret.
type SavedSecret = (
    &'static str,
    &'static str,
    Vec<(&'static str, Value)>,
    &'static str,
);

/// A registry with saved secrets, answering as the real one's rule does:
/// only when every endpoint given matches the saved one.
#[derive(Debug, Default)]
struct Secrets {
    saved: Vec<SavedSecret>,
    asked: Mutex<Vec<String>>,
}

impl SettingsApi for Secrets {
    fn index(&self) -> SettingsIndex {
        unimplemented!()
    }
    fn section(&self, _: &str) -> Option<SectionValues> {
        unimplemented!()
    }
    fn save(&self, _: SettingsBatch) -> Result<SavedBatch, SettingsFailure> {
        unimplemented!()
    }
    fn preview(&self, _: SettingsBatch) -> Result<SettingsPreview, SettingsFailure> {
        unimplemented!()
    }
    fn export(&self, _: bool) -> String {
        unimplemented!()
    }
    fn import(&self, _: SettingsImport, _: bool) -> Result<Imported, SettingsFailure> {
        unimplemented!()
    }
    fn saved_secret(
        &self,
        section: &str,
        key: &str,
        endpoints: &[(&str, Value)],
    ) -> Option<String> {
        self.asked.lock().unwrap().push(format!("{section}.{key}"));
        self.saved
            .iter()
            .find(|(s, k, want, _)| {
                *s == section
                    && *k == key
                    && want
                        .iter()
                        .all(|(n, v)| endpoints.iter().any(|(e, x)| e == n && x == v))
            })
            .map(|(.., secret)| secret.to_string())
    }
}

fn api(s: Secrets) -> Option<Arc<dyn SettingsApi>> {
    Some(Arc::new(s))
}

fn code(r: &SettingsTestResult) -> &str {
    r.error.as_ref().map_or("ok", |e| e.code.as_str())
}

#[tokio::test]
async fn torznab_asks_for_caps_with_the_key_and_reports_the_version() {
    let seen = Received::default();
    let addr = torznab_server(seen.clone()).await;
    let url = format!("http://{addr}/api?apikey=leaked&t=search");
    let test = |key: Option<&str>| {
        SettingsTest::Torznab(TorznabDraft {
            api_key: key.map(str::to_string),
            url: url.clone(),
        })
    };
    let r = run(test(Some("k1")), None, Net::default()).await;
    assert_eq!((code(&r), r.version.as_deref()), ("ok", Some("1.4.2")));
    // The form's own query is dropped; only the test's parameters go.
    assert_eq!(seen.lock().unwrap()[0].0, "t=caps&apikey=k1");
    let r = run(test(Some("wrong")), None, Net::default()).await;
    assert_eq!(code(&r), "bad_credentials");
    assert!(
        !format!("{r:?}").contains("Incorrect"),
        "the server's words aren't repeated"
    );
    // Left out: the saved key, but only for the saved server.
    let saved = || Secrets {
        saved: vec![(
            "integrations",
            "torznab_api_key",
            vec![("torznab_url", json!(url.clone()))],
            "k1",
        )],
        ..Default::default()
    };
    let r = run(test(None), api(saved()), Net::default()).await;
    assert_eq!(code(&r), "ok");
    let other = SettingsTest::Torznab(TorznabDraft {
        api_key: None,
        url: format!("http://{addr}/api"),
    });
    let r = run(other, api(saved()), Net::default()).await;
    assert_eq!(
        code(&r),
        "bad_credentials",
        "no saved key for another address"
    );
}

#[tokio::test]
async fn ntfy_and_the_webhook_send_one_labelled_test() {
    let seen = Received::default();
    let addr = sink(seen.clone()).await;
    let r = run(
        SettingsTest::Ntfy(NtfyDraft {
            allow_private: true,
            priority: 0,
            token: Some("tk_abc".into()),
            // A local name: a token may cross plain HTTP only to one.
            url: Some(format!("http://ntfy:{}/topic", addr.port())),
        }),
        None,
        local(),
    )
    .await;
    assert_eq!(code(&r), "ok", "{r:?}");
    let r = run(
        SettingsTest::Webhook(WebhookDraft {
            allow_private: false,
            url: Some(format!("http://{addr}/hook")),
        }),
        None,
        local(),
    )
    .await;
    assert_eq!(code(&r), "ok", "{r:?}");
    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].0, "Bearer tk_abc");
    assert!(got[0].1.contains("test from Lodestar"), "{}", got[0].1);
    assert!(got[1].1.contains("\"test\""), "{}", got[1].1);
}

#[tokio::test]
async fn a_saved_ntfy_token_goes_only_to_the_saved_server() {
    let seen = Received::default();
    let addr = sink(seen.clone()).await;
    let saved_url = format!("http://ntfy:{}/topic", addr.port());
    let secrets = || Secrets {
        saved: vec![
            ("notifications", "ntfy_url", vec![], "unused"),
            (
                "notifications",
                "ntfy_token",
                vec![("ntfy_url", json!(saved_url.clone()))],
                "tk_saved",
            ),
        ],
        ..Default::default()
    };
    let draft = |url: String| {
        SettingsTest::Ntfy(NtfyDraft {
            allow_private: true,
            priority: 0,
            token: None,
            url: Some(url),
        })
    };
    run(draft(saved_url.clone()), api(secrets()), local()).await;
    // Another topic URL: the token isn't sent.
    let other = format!("http://elsewhere:{}/topic", addr.port());
    run(draft(other), api(secrets()), local()).await;
    let got = seen.lock().unwrap().clone();
    assert_eq!(got[0].0, "Bearer tk_saved");
    assert_eq!(got[1].0, "", "no token to another server");
}

#[tokio::test]
async fn the_ssrf_guard_refuses_private_and_link_local_targets() {
    for (test, net) in [
        (
            SettingsTest::Ntfy(NtfyDraft {
                allow_private: false,
                priority: 0,
                token: None,
                url: Some("http://127.0.0.1:9/topic".into()),
            }),
            Net::default(),
        ),
        (
            SettingsTest::Webhook(WebhookDraft {
                allow_private: false,
                url: Some("http://hooks.example/x".into()),
            }),
            to("10.0.0.5"),
        ),
        (
            SettingsTest::Webhook(WebhookDraft {
                allow_private: true,
                url: Some("http://hooks.example/x".into()),
            }),
            to("169.254.169.254"),
        ),
        (
            SettingsTest::Discord(DiscordDraft {
                url: Some("https://discord.com/api/webhooks/123456789012345678/abc".into()),
            }),
            to("169.254.169.254"),
        ),
        (
            SettingsTest::Email(EmailDraft {
                allow_private: true,
                from: "me@example.com".into(),
                host: "smtp.example.com".into(),
                password: None,
                port: 0,
                security: EmailSecurity::Tls,
                to: vec!["me@example.com".into()],
                username: String::new(),
            }),
            to("169.254.169.254"),
        ),
        (
            SettingsTest::Torznab(TorznabDraft {
                api_key: Some("k".into()),
                url: "http://indexer.example/api".into(),
            }),
            to("169.254.169.254"),
        ),
        (
            SettingsTest::DownloadClient(DownloadClientDraft {
                client: crate::settings::DownloadClientKind::Qbittorrent,
                id: None,
                password: Some("p".into()),
                url: "http://169.254.169.254/".into(),
                username: Some("u".into()),
            }),
            Net::default(),
        ),
    ] {
        let shown = format!("{test:?}");
        let r = run(test, None, net).await;
        assert_eq!(code(&r), "blocked", "{shown}: {r:?}");
    }
}

#[tokio::test]
async fn a_missing_secret_with_nothing_saved_says_what_to_enter() {
    let r = run(
        SettingsTest::Discord(DiscordDraft { url: None }),
        None,
        Net::default(),
    )
    .await;
    assert_eq!(code(&r), "invalid");
    let r = run(
        SettingsTest::Discord(DiscordDraft {
            url: Some("https://example.com/api/webhooks/1/x".into()),
        }),
        None,
        Net::default(),
    )
    .await;
    assert_eq!(code(&r), "invalid", "only Discord's own webhook URLs");
    // A saved client's password is asked for by its id, with its address.
    let secrets = Secrets::default();
    let s = Arc::new(secrets);
    let api: Arc<dyn SettingsApi> = s.clone();
    run(
        SettingsTest::DownloadClient(DownloadClientDraft {
            client: crate::settings::DownloadClientKind::Qbittorrent,
            id: Some("home".into()),
            password: None,
            url: "http://169.254.169.254/".into(),
            username: Some("u".into()),
        }),
        Some(api),
        Net::default(),
    )
    .await;
    assert_eq!(
        *s.asked.lock().unwrap(),
        ["integrations.clients.home.password"]
    );
}

#[test]
fn every_kind_bounds_its_fields() {
    let long = "a".repeat(MAX_TEST_FIELD + 1);
    for t in [
        SettingsTest::Torznab(TorznabDraft {
            api_key: Some(long.clone()),
            url: "http://x/".into(),
        }),
        SettingsTest::Ntfy(NtfyDraft {
            allow_private: false,
            priority: 0,
            token: None,
            url: Some(long.clone()),
        }),
        SettingsTest::Email(EmailDraft {
            allow_private: false,
            from: "a@b.c".into(),
            host: "h".into(),
            password: None,
            port: 0,
            security: EmailSecurity::Tls,
            to: vec![long.clone()],
            username: String::new(),
        }),
    ] {
        assert!(too_long(&t));
    }
}
