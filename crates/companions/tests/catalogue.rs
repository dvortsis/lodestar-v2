//! The catalogue contract: the compiled-in `data/catalogue.json` validates
//! and holds the v0.6 entries the owner chose (§2.47, cq6); a pinned
//! catalogue is read correctly; every rule refuses what it should, loudly
//! and without panicking; and no mutation of a valid catalogue slips past
//! validation into something weaker.

use lodestar_companions::catalogue::{
    Arch, ArchiveKind, Catalogue, DOWNLOAD_HOSTS, Exposure, MAX_ARTIFACT_SIZE, MAX_JSON, Need,
    NotAvailable, Platform, Proto, REGISTRIES, Rule, SUPPORTED, TemplateId, UpstreamSig, builtin,
};
use proptest::prelude::*;
use serde_json::{Value, json};

const BUILTIN: &str = include_str!("../data/catalogue.json");

const SHA: &str = "4f9a3c1e7d2b6a8f0c5e9d3b7a1f4c8e2d6b0a9f3c7e1d5b9a2f6c0e4d8b3a71";
const SHA2: &str = "0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c";

/// A fully pinned catalogue, the shape CMP-13's tool will write.
fn pinned() -> Value {
    json!({
        "format": 1,
        "entries": [
            {
                "id": "qbittorrent",
                "name": "qBittorrent",
                "licence": {"spdx": "GPL-2.0-or-later", "url": "https://github.com/qbittorrent/qBittorrent/blob/master/COPYING"},
                "homepage": "https://www.qbittorrent.org/",
                "needed_for": ["download_client"],
                "opt_in": false,
                "hosts": ["github.com", "release-assets.githubusercontent.com"],
                "native": {"release": "https://github.com/userdocs/qbittorrent-nox-static/releases/download/", "artifacts": [
                    {"platform": "linux", "arch": "x86_64", "archive": "binary", "executable": null,
                     "pin": {"version": "5.0.3",
                             "url": "https://github.com/userdocs/qbittorrent-nox-static/releases/download/release-5.0.3_v2.0.11/x86_64-qbittorrent-nox",
                             "size": 31_000_000, "sha256": SHA, "upstream_sig": {"kind": "none"}}},
                    {"platform": "linux", "arch": "aarch64", "archive": "binary", "executable": null, "pin": null}
                ]},
                "container": {"image": "lscr.io/linuxserver/qbittorrent", "digest": format!("sha256:{SHA2}"), "name": "qbittorrent"},
                "ports": [
                    {"name": "webui", "port": 8080, "proto": "tcp", "exposure": "internal"},
                    {"name": "p2p", "port": 6881, "proto": "tcp", "exposure": "p2p"},
                    {"name": "p2p_udp", "port": 6881, "proto": "udp", "exposure": "p2p"}
                ],
                "health": {"port": "webui", "path": "/api/v2/app/version"},
                "templates": ["qbittorrent_conf", "compose_service", "unraid_template"]
            },
            {
                "id": "prowlarr",
                "name": "Prowlarr",
                "licence": {"spdx": "GPL-3.0-only", "url": "https://github.com/Prowlarr/Prowlarr/blob/develop/LICENSE.md"},
                "homepage": "https://prowlarr.com/",
                "needed_for": ["indexer_manager"],
                "opt_in": false,
                "hosts": ["github.com"],
                "native": {"release": "https://github.com/Prowlarr/Prowlarr/releases/download/", "artifacts": [
                    {"platform": "windows", "arch": "x86_64", "archive": "zip", "executable": "Prowlarr/Prowlarr.exe",
                     "pin": {"version": "1.28.2.4885",
                             "url": "https://github.com/Prowlarr/Prowlarr/releases/download/v1.28.2.4885/Prowlarr.master.1.28.2.4885.windows-core-x64.zip",
                             "size": 60_000_000, "sha256": SHA2,
                             "upstream_sig": {"kind": "verified_at_pin", "fingerprint": "0123456789ABCDEF0123456789ABCDEF01234567"}}},
                    {"platform": "linux", "arch": "x86_64", "archive": "tar_gz", "executable": "Prowlarr/Prowlarr",
                     "pin": {"version": "1.28.2.4885",
                             "url": "https://github.com/Prowlarr/Prowlarr/releases/download/v1.28.2.4885/Prowlarr.master.1.28.2.4885.linux-core-x64.tar.gz",
                             "size": 60_000_000, "sha256": SHA, "upstream_sig": {"kind": "none"}}}
                ]},
                "container": null,
                "ports": [{"name": "webui", "port": 9696, "proto": "tcp", "exposure": "internal"}],
                "health": {"port": "webui", "path": "/ping"},
                "templates": ["prowlarr_config_xml"]
            },
            {
                "id": "flaresolverr",
                "name": "FlareSolverr",
                "licence": {"spdx": "MIT", "url": "https://github.com/FlareSolverr/FlareSolverr/blob/master/LICENSE"},
                "homepage": "https://github.com/FlareSolverr/FlareSolverr",
                "needed_for": ["challenge_solver"],
                "opt_in": true,
                "hosts": [],
                "container": {"image": "ghcr.io/flaresolverr/flaresolverr", "digest": null, "name": "flaresolverr"},
                "ports": [{"name": "api", "port": 8191, "proto": "tcp", "exposure": "internal"}],
                "health": null,
                "templates": ["compose_service"]
            }
        ]
    })
}

fn parse(v: &Value) -> Result<Catalogue, lodestar_companions::CatalogueError> {
    Catalogue::from_json(&v.to_string())
}

/// Applies `f` to a copy of the pinned catalogue and expects `rule`.
fn refused(rule: Rule, f: impl FnOnce(&mut Value)) {
    let mut v = pinned();
    f(&mut v);
    match parse(&v) {
        Ok(_) => panic!("accepted a catalogue that breaks {rule:?}"),
        Err(e) => assert_eq!(e.rule(), rule, "{e}"),
    }
}

fn qbit(v: &mut Value) -> &mut Value {
    &mut v["entries"][0]
}

fn qbit_pin(v: &mut Value) -> &mut Value {
    &mut v["entries"][0]["native"]["artifacts"][0]["pin"]
}

fn prowlarr_zip(v: &mut Value) -> &mut Value {
    &mut v["entries"][1]["native"]["artifacts"][0]
}

#[test]
fn builtin_validates() {
    let c = builtin().unwrap_or_else(|e| panic!("data/catalogue.json: {e}"));
    let ids: Vec<&str> = c.entries().iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["qbittorrent", "prowlarr", "gluetun"]);
    // The same text through the test loader gives the same answer.
    assert!(Catalogue::from_json(BUILTIN).is_ok());
}

/// Owner §2.47 (cq6): whose binaries run, and Q4/Q21's exclusions.
#[test]
fn builtin_entries_follow_the_owner_decisions() {
    let c = builtin().unwrap();
    let q = c.get("qbittorrent").unwrap();
    // userdocs static qbittorrent-nox on Linux; on Windows, upstream's
    // installer, which Lodestar only shows (owner cq4, CMP-11).
    assert!(q.artifact(Platform::Linux, Arch::X86_64).is_some());
    assert!(q.artifact(Platform::Linux, Arch::Aarch64).is_some());
    for a in &q.native.as_ref().unwrap().artifacts {
        let want = match a.platform {
            Platform::Linux => ArchiveKind::Binary,
            Platform::Windows => ArchiveKind::Installer,
        };
        assert_eq!(a.archive, want);
    }
    let w = q.artifact(Platform::Windows, Arch::X86_64).unwrap();
    assert!(w.executable.is_none());
    assert_eq!(
        q.installable(Platform::Windows, Arch::X86_64).unwrap_err(),
        NotAvailable::AwaitingPin
    );
    // The image itself is readable only once pinned (the crate's own
    // unit test checks it's lscr.io/linuxserver/qbittorrent).
    assert!(q.container.is_some());

    let p = c.get("prowlarr").unwrap();
    assert_eq!(p.native.as_ref().unwrap().artifacts.len(), 3);
    // The image itself is readable only once pinned (the crate's own
    // unit test checks it's lscr.io/linuxserver/prowlarr).
    assert!(p.container.is_some());

    let g = c.get("gluetun").unwrap();
    assert!(g.native.is_none(), "the native VPN is v1.x");
    // The image itself is readable only once pinned (the crate's own
    // unit test checks it's docker.io/qmcgaw/gluetun).
    assert!(g.container.is_some());
    assert!(g.needs(Need::VpnRoute));

    // FlareSolverr arrives with CMP-12, opt-in.
    assert!(c.get("flaresolverr").is_none());
    for e in c.entries() {
        assert!(!e.opt_in, "{}: v0.6's default entries are not opt-in", e.id);
        for h in &e.hosts {
            assert!(DOWNLOAD_HOSTS.contains(&h.as_str()));
        }
        if let Some(ct) = &e.container {
            assert_eq!(ct.name().as_str(), e.id.as_str());
        }
        // Every non-P2P port is internal, and only the download client
        // has P2P ports.
        for port in &e.ports {
            assert!(port.exposure == Exposure::Internal || e.needs(Need::DownloadClient));
        }
    }
}

/// Pins come from CMP-13's tool. Until then every compiled-in entry is
/// awaiting its pin, and the APIs CMP-1, CMP-5 and CMP-7 use refuse it
/// with a clear error: the fail-safe state.
#[test]
fn builtin_entries_await_their_pins_and_are_refused() {
    let c = builtin().unwrap();
    for e in c.entries() {
        assert!(e.awaiting_pin(), "{}: v0.6 entries ship unpinned", e.id);
        for &(platform, arch) in SUPPORTED {
            match e.installable(platform, arch) {
                Err(NotAvailable::AwaitingPin) => {
                    assert!(e.artifact(platform, arch).is_some())
                }
                Err(NotAvailable::NoPlatform) => assert!(e.artifact(platform, arch).is_none()),
                Err(NotAvailable::NoBuild) => assert!(e.native.is_none()),
                Err(other) => panic!("{}: {other:?}", e.id),
                Ok(_) => panic!("{}: installable before CMP-13 pinned it", e.id),
            }
        }
        match e.renderable() {
            Err(NotAvailable::AwaitingPin) => assert!(e.container.is_some()),
            Err(NotAvailable::NoBuild) => assert!(e.container.is_none()),
            Err(other) => panic!("{}: {other:?}", e.id),
            Ok(_) => panic!("{}: renderable before CMP-13 pinned it", e.id),
        }
    }
    // The errors say why, plainly.
    let q = c.get("qbittorrent").unwrap();
    let e = q.installable(Platform::Linux, Arch::X86_64).unwrap_err();
    assert_eq!(e, NotAvailable::AwaitingPin);
    assert!(e.to_string().contains("isn't pinned"));
    assert_eq!(
        c.get("gluetun")
            .unwrap()
            .installable(Platform::Linux, Arch::X86_64)
            .unwrap_err(),
        NotAvailable::NoBuild
    );
}

/// When pins land, each is well-formed on the reading side too.
#[test]
fn builtin_pins_are_well_formed_where_present() {
    let c = builtin().unwrap();
    for e in c.entries() {
        for &(platform, arch) in SUPPORTED {
            if let Ok((a, pin)) = e.installable(platform, arch) {
                assert_eq!(a.platform, platform);
                assert!(e.allows_host(pin.url.host()));
                assert!(pin.size > 0 && pin.size <= MAX_ARTIFACT_SIZE);
                assert_eq!(pin.sha256.to_hex().len(), 64);
            }
        }
        if let Ok((_, image)) = e.renderable() {
            assert!(REGISTRIES.contains(&image.registry()));
            assert!(image.reference().contains("@sha256:"));
        }
    }
}

#[test]
fn reads_a_pinned_catalogue() {
    let c = parse(&pinned()).unwrap();
    let q = c.get("qbittorrent").unwrap();
    let (a, pin) = q.installable(Platform::Linux, Arch::X86_64).unwrap();
    assert_eq!(a.archive, ArchiveKind::Binary);
    assert!(a.executable.is_none());
    assert_eq!(pin.version.as_str(), "5.0.3");
    assert_eq!(pin.url.host(), "github.com");
    assert_eq!(pin.url.file_name(), "x86_64-qbittorrent-nox");
    assert_eq!(pin.size, 31_000_000);
    assert_eq!(pin.sha256.to_hex(), SHA);
    assert_eq!(pin.upstream_sig, UpstreamSig::None);
    // Unpinned aarch64: present, not installable.
    assert!(q.artifact(Platform::Linux, Arch::Aarch64).is_some());
    assert_eq!(
        q.installable(Platform::Linux, Arch::Aarch64).unwrap_err(),
        NotAvailable::AwaitingPin
    );
    assert_eq!(
        q.installable(Platform::Windows, Arch::X86_64).unwrap_err(),
        NotAvailable::NoPlatform
    );
    assert!(q.awaiting_pin());
    assert!(q.allows_host("release-assets.githubusercontent.com"));
    assert!(!q.allows_host("objects.githubusercontent.com"));
    let (_, image) = q.renderable().unwrap();
    assert_eq!(image.digest().to_hex(), SHA2);
    // Rendered only as image@digest, never by tag.
    assert_eq!(
        image.reference(),
        format!("lscr.io/linuxserver/qbittorrent@sha256:{SHA2}")
    );
    assert_eq!(image.to_string(), image.reference());
    let webui = q.port("webui").unwrap();
    assert_eq!(
        (webui.number.get(), webui.proto, webui.exposure),
        (8080, Proto::Tcp, Exposure::Internal)
    );
    assert_eq!(
        q.health.as_ref().unwrap().path.as_str(),
        "/api/v2/app/version"
    );
    assert!(q.templates.contains(&TemplateId::QbittorrentConf));

    let p = c.get("prowlarr").unwrap();
    let (a, pin) = p.installable(Platform::Windows, Arch::X86_64).unwrap();
    assert_eq!(
        a.executable.as_ref().unwrap().as_str(),
        "Prowlarr/Prowlarr.exe"
    );
    match &pin.upstream_sig {
        UpstreamSig::VerifiedAtPin { fingerprint } => {
            assert_eq!(
                fingerprint.as_str(),
                "0123456789ABCDEF0123456789ABCDEF01234567"
            )
        }
        other => panic!("{other:?}"),
    }
    assert!(p.container.is_none());
    assert_eq!(p.renderable().unwrap_err(), NotAvailable::NoBuild);
    assert!(!p.awaiting_pin());

    let f = c.get("flaresolverr").unwrap();
    assert!(f.opt_in);
    assert_eq!(f.renderable().unwrap_err(), NotAvailable::AwaitingPin);
}

#[test]
fn refuses_syntax_and_shape() {
    for bad in ["", "{", "null", "[]", "{\"format\": 1}", "\u{feff}{}"] {
        let e = Catalogue::from_json(bad).unwrap_err();
        assert_eq!(e.rule(), Rule::Syntax, "{bad:?}: {e}");
        assert!(e.at().starts_with("line "));
    }
    // Duplicate keys, unknown keys anywhere, and fractions or negatives.
    let dup = BUILTIN.replacen("\"format\": 1,", "\"format\": 1, \"format\": 1,", 1);
    assert_eq!(Catalogue::from_json(&dup).unwrap_err().rule(), Rule::Syntax);
    refused(Rule::Syntax, |v| v["extra"] = json!(1));
    refused(Rule::Syntax, |v| qbit(v)["extra"] = json!(1));
    refused(Rule::Syntax, |v| qbit_pin(v)["extra"] = json!(1));
    refused(Rule::Syntax, |v| {
        qbit_pin(v)["upstream_sig"]["extra"] = json!(1)
    });
    refused(Rule::Syntax, |v| qbit(v)["ports"][0]["extra"] = json!(1));
    refused(Rule::Syntax, |v| {
        qbit(v)["container"]["tag"] = json!("latest")
    });
    refused(Rule::Syntax, |v| qbit_pin(v)["size"] = json!(1.5));
    refused(Rule::Syntax, |v| qbit_pin(v)["size"] = json!(-1));
    refused(Rule::Syntax, |v| qbit(v)["ports"][0]["port"] = json!(70000));
    refused(Rule::Syntax, |v| qbit(v)["opt_in"] = json!("false"));
    // Closed vocabularies.
    refused(Rule::Syntax, |v| qbit(v)["needed_for"] = json!(["miner"]));
    refused(Rule::Syntax, |v| {
        qbit(v)["templates"] = json!(["shell_script"])
    });
    refused(Rule::Syntax, |v| {
        qbit(v)["native"]["artifacts"][0]["platform"] = json!("macos")
    });
    refused(Rule::Syntax, |v| {
        qbit(v)["native"]["artifacts"][0]["arch"] = json!("riscv64")
    });
    refused(Rule::Syntax, |v| {
        qbit(v)["native"]["artifacts"][0]["archive"] = json!("tar.xz")
    });
    refused(Rule::Syntax, |v| {
        qbit_pin(v)["upstream_sig"]["kind"] = json!("trusted")
    });
    refused(Rule::Syntax, |v| {
        qbit(v)["ports"][0]["exposure"] = json!("public")
    });
    // Missing required fields.
    refused(Rule::Syntax, |v| {
        qbit_pin(v).as_object_mut().unwrap().remove("sha256");
    });
    refused(Rule::Syntax, |v| {
        qbit(v).as_object_mut().unwrap().remove("hosts");
    });
}

/// The VPN route's service is `service:gluetun`, so its name is fixed; and
/// extensions compare without case.
#[test]
fn refuses_a_renamed_vpn_service_and_cased_extensions() {
    let renamed = BUILTIN.replacen("\"name\": \"gluetun\"}", "\"name\": \"vpn\"}", 1);
    assert_ne!(renamed, BUILTIN);
    let e = Catalogue::from_json(&renamed).unwrap_err();
    assert_eq!(e.rule(), Rule::Name, "{e}");
    assert_eq!(e.at(), "entries[2].container.name");

    refused(Rule::Archive, |v| {
        qbit_pin(v)["url"] = json!("https://github.com/x/qbittorrent.EXE");
    });
    refused(Rule::Executable, |v| {
        v["entries"][1]["native"]["artifacts"][1]["executable"] = json!("Prowlarr/Prowlarr.EXE");
    });
    let mut v = pinned();
    prowlarr_zip(&mut v)["executable"] = json!("Prowlarr/Prowlarr.EXE");
    prowlarr_zip(&mut v)["pin"]["url"] = json!(
        "https://github.com/Prowlarr/Prowlarr/releases/download/v1.28.2.4885/Prowlarr.master.1.28.2.4885.windows-core-x64.ZIP"
    );
    assert!(parse(&v).is_ok());
}

/// cq6: pins come from the entry's own release folder, never another owner
/// or project on the same host.
#[test]
fn refuses_pins_from_another_source() {
    refused(Rule::Source, |v| {
        qbit_pin(v)["url"] = json!(
            "https://github.com/anyone/qbittorrent-nox-static/releases/download/release-5.0.3_v2.0.11/x86_64-qbittorrent-nox"
        );
    });
    refused(Rule::Source, |v| {
        qbit_pin(v)["url"] = json!(
            "https://github.com/userdocs/other-project/releases/download/r1/x86_64-qbittorrent-nox"
        );
    });
    refused(Rule::Source, |v| {
        prowlarr_zip(v)["pin"]["url"] =
            json!("https://github.com/Prowlarr/Prowlarr/archive/refs/tags/Prowlarr.zip");
    });
    // The release folder must be on the entry's hosts, and a folder.
    refused(Rule::HostNotAllowed, |v| {
        qbit(v)["native"]["release"] = json!("https://objects.githubusercontent.com/a/b/c/");
    });
    refused(Rule::Source, |v| {
        qbit(v)["native"]["release"] = json!("https://github.com/userdocs/");
    });
    refused(Rule::Source, |v| {
        qbit(v)["native"]["release"] =
            json!("https://github.com/userdocs/qbittorrent-nox-static/releases/download");
    });
    // Exactly `/<owner>/<project>/releases/download/`: not source
    // archives, not another folder under the project, nothing before
    // `releases` and nothing after `download`.
    for bad in [
        "https://github.com/userdocs/qbittorrent-nox-static/archive/",
        "https://github.com/userdocs/qbittorrent-nox-static/releases/",
        "https://github.com/userdocs/qbittorrent-nox-static/tree/releases/download/",
        "https://github.com/userdocs/qbittorrent-nox-static/x/releases/download/",
        "https://github.com/userdocs/qbittorrent-nox-static/releases/download/release-5.0.3_v2.0.11/",
        "https://github.com/userdocs/releases/download/",
        "https://github.com/userdocs/qbittorrent-nox-static/Releases/Download/",
    ] {
        // The pin moves into the bad folder too, so only the folder rule
        // can refuse it.
        refused(Rule::Source, |v| {
            qbit(v)["native"]["release"] = json!(bad);
            qbit_pin(v)["url"] = json!(format!("{bad}v1/x86_64-qbittorrent-nox"));
        });
    }
    // A pin is one release's file, `<tag>/<file>`, nothing deeper.
    refused(Rule::Source, |v| {
        qbit_pin(v)["url"] = json!(
            "https://github.com/userdocs/qbittorrent-nox-static/releases/download/x86_64-qbittorrent-nox"
        );
    });
    refused(Rule::Source, |v| {
        qbit_pin(v)["url"] = json!(
            "https://github.com/userdocs/qbittorrent-nox-static/releases/download/r/sub/x86_64-qbittorrent-nox"
        );
    });
    refused(Rule::Url, |v| {
        qbit(v)["native"]["release"] = json!("http://github.com/a/b/c/")
    });
    refused(Rule::Syntax, |v| {
        qbit(v)["native"].as_object_mut().unwrap().remove("release");
    });
}

/// Q21: FlareSolverr is opt-in, whatever it's labelled.
#[test]
fn refuses_flaresolverr_made_default_by_labelling() {
    let as_indexer = |v: &mut Value, id: &str, name: &str, image: &str| {
        let e = &mut v["entries"][2];
        e["id"] = json!(id);
        e["needed_for"] = json!(["indexer_manager"]);
        e["opt_in"] = json!(false);
        e["container"]["name"] = json!(name);
        e["container"]["image"] = json!(image);
    };
    refused(Rule::Need, |v| {
        as_indexer(v, "flaresolverr", "solver", "ghcr.io/someone/solver")
    });
    refused(Rule::Need, |v| {
        as_indexer(v, "solver", "flaresolverr", "ghcr.io/someone/solver")
    });
    refused(Rule::Need, |v| {
        as_indexer(v, "solver", "solver", "ghcr.io/flaresolverr/flaresolverr")
    });
    refused(Rule::Need, |v| {
        as_indexer(v, "solver", "solver", "docker.io/someone/flaresolverr")
    });
    // Opt-in but not a challenge solver: still refused.
    refused(Rule::Need, |v| {
        as_indexer(
            v,
            "flaresolverr",
            "flaresolverr",
            "ghcr.io/flaresolverr/flaresolverr",
        );
        v["entries"][2]["opt_in"] = json!(true);
    });
    // Opt-in and a challenge solver, but labelled as something else too:
    // refused, so it can't pick up another role's defaults.
    refused(Rule::Need, |v| {
        v["entries"][2]["needed_for"] = json!(["challenge_solver", "indexer_manager"]);
    });
    // The real thing, opt-in and a challenge solver, is fine.
    assert!(parse(&pinned()).is_ok());
    // Names are compared whole: one that merely contains the word is
    // another companion, and needn't be opt-in.
    let mut v = pinned();
    as_indexer(
        &mut v,
        "my-flaresolverr-fork",
        "my-flaresolverr-fork",
        "ghcr.io/someone/my-flaresolverr-fork",
    );
    assert!(parse(&v).is_ok());
    let mut v = pinned();
    as_indexer(&mut v, "solver", "solver", "ghcr.io/flaresolverr/solver");
    assert!(parse(&v).is_ok());
}

/// A companion's own config template goes only with that kind of companion.
#[test]
fn refuses_config_templates_on_the_wrong_companion() {
    refused(Rule::Template, |v| {
        v["entries"][1]["templates"] = json!(["qbittorrent_conf"]);
    });
    refused(Rule::Template, |v| {
        qbit(v)["templates"] = json!(["prowlarr_config_xml"]);
    });
    // The role matches but the id doesn't: another download client or
    // indexer manager isn't qBittorrent or Prowlarr.
    refused(Rule::Template, |v| {
        qbit(v)["id"] = json!("transmission");
        qbit(v)["container"]["name"] = json!("transmission");
    });
    refused(Rule::Template, |v| {
        v["entries"][1]["id"] = json!("jackett");
    });
}

/// CMP-5g: other crates' `tests/` fakes build a plan through the
/// `testing` feature; a shipped build can't (`ConfigPlan`'s doctest).
#[test]
fn tests_can_build_a_config_plan() {
    use lodestar_companions::CompanionId;
    use lodestar_companions::adapter::ConfigPlan;
    let plan = ConfigPlan::for_tests(CompanionId::parse("prowlarr").unwrap(), Vec::new());
    assert_eq!(plan.companion().as_str(), "prowlarr");
    assert!(plan.changes().is_empty());
}

#[test]
fn refuses_each_rule() {
    refused(Rule::Format, |v| v["format"] = json!(2));
    refused(Rule::Count, |v| v["entries"] = json!([]));
    refused(Rule::Duplicate, |v| {
        let mut e = v["entries"][1].clone();
        e["container"] = Value::Null;
        e["templates"] = json!(["prowlarr_config_xml"]);
        v["entries"].as_array_mut().unwrap().push(e);
    });
    refused(Rule::Duplicate, |v| {
        v["entries"][2]["container"]["name"] = json!("qbittorrent");
    });
    refused(Rule::Id, |v| qbit(v)["id"] = json!("qBittorrent"));
    refused(Rule::Id, |v| qbit(v)["id"] = json!("../x"));
    refused(Rule::Name, |v| qbit(v)["name"] = json!("<script>"));
    refused(Rule::Name, |v| qbit(v)["name"] = json!(""));
    refused(Rule::Name, |v| qbit(v)["container"]["name"] = json!("Qbit"));
    refused(Rule::Licence, |v| {
        qbit(v)["licence"]["spdx"] = json!("GPL; rm -rf")
    });
    refused(Rule::Url, |v| {
        qbit(v)["licence"]["url"] = json!("http://example.com/")
    });
    refused(Rule::Url, |v| {
        qbit(v)["homepage"] = json!("javascript:alert(1)")
    });

    // Hosts: grammar, the global allowlist, and the entry's own list.
    refused(Rule::Host, |v| qbit(v)["hosts"][0] = json!("GitHub.com"));
    refused(Rule::HostNotAllowed, |v| {
        qbit(v)["hosts"][0] = json!("evil.example")
    });
    refused(Rule::Duplicate, |v| {
        qbit(v)["hosts"][1] = json!("github.com")
    });
    refused(Rule::Count, |v| {
        qbit(v)["hosts"] = json!(vec!["github.com"; 9])
    });
    refused(Rule::HostNotAllowed, |v| {
        qbit_pin(v)["url"] =
            json!("https://objects.githubusercontent.com/x/x86_64-qbittorrent-nox");
    });
    refused(Rule::Url, |v| {
        qbit_pin(v)["url"] = json!("http://github.com/x/x86_64-qbittorrent-nox")
    });
    refused(Rule::Url, |v| {
        qbit_pin(v)["url"] = json!("https://github.com/x/%2e%2e/x86_64-qbittorrent-nox")
    });
    refused(Rule::Url, |v| {
        qbit_pin(v)["url"] = json!("https://github.com:8443/x/x86_64-qbittorrent-nox")
    });
    refused(Rule::Url, |v| {
        qbit_pin(v)["url"] = json!("https://u@github.com/x/x86_64-qbittorrent-nox")
    });
    // Hosts exist only for native downloads.
    refused(Rule::Shape, |v| {
        v["entries"][2]["hosts"] = json!(["github.com"])
    });
    // Native builds with no hosts: their release folder's host isn't
    // allowed, which is checked first.
    refused(Rule::HostNotAllowed, |v| {
        qbit(v)["hosts"] = json!([]);
        *qbit_pin(v) = Value::Null;
    });
    // Something to install or render.
    refused(Rule::Shape, |v| {
        let e = &mut v["entries"][2];
        e["container"] = Value::Null;
        e["templates"] = json!([]);
    });

    // Platforms v0.6 doesn't support.
    refused(Rule::Platform, |v| {
        let a = prowlarr_zip(v);
        a["arch"] = json!("aarch64");
    });
    refused(Rule::Duplicate, |v| {
        let a = qbit(v)["native"]["artifacts"][0].clone();
        qbit(v)["native"]["artifacts"]
            .as_array_mut()
            .unwrap()
            .push(a);
    });
    refused(Rule::Count, |v| qbit(v)["native"]["artifacts"] = json!([]));

    // Archive kind, executable and URL agree.
    refused(Rule::Executable, |v| {
        qbit(v)["native"]["artifacts"][0]["executable"] = json!("x")
    });
    refused(Rule::Executable, |v| {
        prowlarr_zip(v)["executable"] = Value::Null
    });
    refused(Rule::Executable, |v| {
        prowlarr_zip(v)["executable"] = json!("../Prowlarr.exe")
    });
    refused(Rule::Executable, |v| {
        prowlarr_zip(v)["executable"] = json!("/Prowlarr.exe")
    });
    refused(Rule::Executable, |v| {
        prowlarr_zip(v)["executable"] = json!("Prowlarr\\Prowlarr.exe")
    });
    refused(Rule::Executable, |v| {
        prowlarr_zip(v)["executable"] = json!("Prowlarr/Prowlarr")
    });
    refused(Rule::Executable, |v| {
        v["entries"][1]["native"]["artifacts"][1]["executable"] = json!("Prowlarr/Prowlarr.exe");
    });
    refused(Rule::Archive, |v| {
        prowlarr_zip(v)["pin"]["url"] =
            json!("https://github.com/Prowlarr/Prowlarr/releases/download/v1/Prowlarr.tar.gz");
    });
    refused(Rule::Archive, |v| {
        qbit_pin(v)["url"] = json!("https://github.com/x/qbittorrent.zip");
    });
    refused(Rule::Archive, |v| {
        qbit_pin(v)["url"] = json!("https://github.com/x/qbittorrent.exe");
    });
    refused(Rule::Archive, |v| {
        qbit_pin(v)["url"] = json!("https://github.com/x/")
    });
    refused(Rule::Archive, |v| {
        prowlarr_zip(v)["pin"]["url"] = json!("https://github.com/x/.zip");
    });
    refused(Rule::Archive, |v| {
        let a = &mut qbit(v)["native"]["artifacts"][0];
        a["archive"] = json!("installer");
        a["pin"]["url"] = json!("https://github.com/x/qbittorrent_setup.exe");
    });

    // The pin itself.
    refused(Rule::Version, |v| qbit_pin(v)["version"] = json!("v5.0.3"));
    refused(Rule::Version, |v| qbit_pin(v)["version"] = json!("latest"));
    refused(Rule::Version, |v| {
        v["entries"][1]["native"]["artifacts"][1]["pin"]["version"] = json!("1.28.3.4900");
    });
    refused(Rule::Size, |v| qbit_pin(v)["size"] = json!(0));
    refused(Rule::Size, |v| {
        qbit_pin(v)["size"] = json!(MAX_ARTIFACT_SIZE + 1)
    });
    refused(Rule::Sha256, |v| {
        qbit_pin(v)["sha256"] = json!(SHA.to_uppercase())
    });
    refused(Rule::Sha256, |v| qbit_pin(v)["sha256"] = json!(&SHA[..63]));
    refused(Rule::Sha256, |v| {
        qbit_pin(v)["sha256"] = json!("0".repeat(64))
    });
    refused(Rule::Sha256, |v| {
        qbit_pin(v)["sha256"] = json!(format!("sha256:{SHA}"))
    });
    refused(Rule::Fingerprint, |v| {
        qbit_pin(v)["upstream_sig"] = json!({"kind": "verified_at_pin"});
    });
    refused(Rule::Fingerprint, |v| {
        qbit_pin(v)["upstream_sig"] =
            json!({"kind": "none", "fingerprint": "0123456789ABCDEF0123456789ABCDEF01234567"});
    });
    refused(Rule::Fingerprint, |v| {
        prowlarr_zip(v)["pin"]["upstream_sig"]["fingerprint"] = json!("0123 4567");
    });

    // Images.
    refused(Rule::Image, |v| {
        qbit(v)["container"]["image"] = json!("evil.example/linuxserver/qbittorrent")
    });
    refused(Rule::Image, |v| {
        qbit(v)["container"]["image"] = json!("lscr.io/linuxserver/qbittorrent:latest")
    });
    refused(Rule::Image, |v| {
        qbit(v)["container"]["image"] = json!("qbittorrent")
    });
    refused(Rule::Sha256, |v| {
        qbit(v)["container"]["digest"] = json!(SHA2)
    });
    refused(Rule::Sha256, |v| {
        qbit(v)["container"]["digest"] = json!(format!("sha512:{SHA2}"))
    });

    // Ports.
    refused(Rule::Port, |v| qbit(v)["ports"][0]["port"] = json!(80));
    refused(Rule::Port, |v| qbit(v)["ports"][0]["port"] = json!(0));
    refused(Rule::PortName, |v| {
        qbit(v)["ports"][0]["name"] = json!("Web UI")
    });
    refused(Rule::Duplicate, |v| {
        qbit(v)["ports"][1]["name"] = json!("webui")
    });
    refused(Rule::Duplicate, |v| {
        qbit(v)["ports"][1]["port"] = json!(8080)
    });
    refused(Rule::Count, |v| qbit(v)["ports"] = json!([]));
    refused(Rule::Exposure, |v| {
        v["entries"][1]["ports"][0]["exposure"] = json!("p2p")
    });

    // Health.
    refused(Rule::Health, |v| qbit(v)["health"]["port"] = json!("nope"));
    refused(Rule::Health, |v| qbit(v)["health"]["port"] = json!("p2p"));
    refused(Rule::Health, |v| {
        qbit(v)["health"]["port"] = json!("p2p_udp")
    });
    refused(Rule::Health, |v| {
        qbit(v)["health"]["path"] = json!("api/v2")
    });
    refused(Rule::Health, |v| {
        qbit(v)["health"]["path"] = json!("/../admin")
    });
    refused(Rule::Health, |v| qbit(v)["health"]["path"] = json!("/x#y"));

    // Templates.
    refused(Rule::Duplicate, |v| {
        qbit(v)["templates"] = json!(["compose_service", "compose_service"])
    });
    refused(Rule::Template, |v| {
        v["entries"][1]["templates"] = json!(["compose_service"])
    });
    refused(Rule::Template, |v| {
        v["entries"][2]["templates"] = json!(["qbittorrent_conf"])
    });

    // Needs: FlareSolverr is opt-in only (Q21); the VPN route is a
    // container (native VPN is v1.x).
    refused(Rule::Need, |v| v["entries"][2]["opt_in"] = json!(false));
    refused(Rule::Count, |v| qbit(v)["needed_for"] = json!([]));
    refused(Rule::Duplicate, |v| {
        qbit(v)["needed_for"] = json!(["download_client", "download_client"])
    });
    refused(Rule::Shape, |v| {
        v["entries"][1]["needed_for"] = json!(["indexer_manager", "vpn_route"])
    });
}

#[test]
fn refuses_oversized_documents() {
    let big = format!("{BUILTIN}{}", " ".repeat(MAX_JSON));
    let e = Catalogue::from_json(&big).unwrap_err();
    assert_eq!(e.rule(), Rule::TooLarge);
    // Deep nesting is refused by the parser, not the stack.
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert!(Catalogue::from_json(&deep[..MAX_JSON.min(deep.len())]).is_err());
    let deep_obj = format!("{{\"format\":1,\"entries\":{}", "[".repeat(10_000));
    assert_eq!(
        Catalogue::from_json(&deep_obj).unwrap_err().rule(),
        Rule::Syntax
    );
}

/// Errors name the rule and a location built from field names and
/// indexes, never catalogue text.
#[test]
fn errors_are_safe_to_show() {
    let mut v = pinned();
    qbit_pin(&mut v)["url"] = json!("https://evil.example/\u{1b}[31mowned");
    let e = parse(&v).unwrap_err();
    assert_eq!(e.at(), "entries[0].native.artifacts[0].pin.url");
    let shown = e.to_string();
    assert!(!shown.contains("evil"), "{shown}");
    assert!(!shown.contains('\u{1b}'));
    assert_eq!(e.rule().code(), "url");

    let mut v = pinned();
    qbit(&mut v)["mystery_field"] = json!("payload");
    let e = parse(&v).unwrap_err();
    assert!(!e.to_string().contains("mystery"), "{e}");
    assert!(!e.to_string().contains("payload"), "{e}");
}

// ─── Property tests ─────────────────────────────────────────────────────

/// Every string-valued leaf of a JSON value, by path.
fn string_leaves(v: &Value, path: &mut Vec<PathStep>, out: &mut Vec<Vec<PathStep>>) {
    match v {
        Value::String(_) => out.push(path.clone()),
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(PathStep::Index(i));
                string_leaves(x, path, out);
                path.pop();
            }
        }
        Value::Object(o) => {
            for (k, x) in o {
                path.push(PathStep::Key(k.clone()));
                string_leaves(x, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone)]
enum PathStep {
    Key(String),
    Index(usize),
}

fn at_mut<'a>(v: &'a mut Value, path: &[PathStep]) -> &'a mut Value {
    path.iter().fold(v, |v, s| match s {
        PathStep::Key(k) => &mut v[k.as_str()],
        PathStep::Index(i) => &mut v[*i],
    })
}

/// Text an attacker would try in any field.
fn hostile() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("https://evil.example/x".to_owned()),
        Just("http://github.com/x".to_owned()),
        Just("../../etc/passwd".to_owned()),
        Just("/etc/passwd".to_owned()),
        Just("a\nb".to_owned()),
        Just("$(id)".to_owned()),
        Just("a:b".to_owned()),
        Just(String::new()),
        Just(" ".to_owned()),
        Just("\u{0}".to_owned()),
        Just("x".repeat(3000)),
        Just("GITHUB.COM".to_owned()),
        Just("sha256:zz".to_owned()),
        "\\PC{0,40}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Replacing any string in a valid catalogue with hostile text either
    /// is refused, or (where the text happens to be valid for that field)
    /// leaves every safety invariant intact. Never a panic.
    #[test]
    fn string_mutations_never_weaken_the_catalogue(pick in any::<prop::sample::Index>(), text in hostile()) {
        let base = pinned();
        let mut leaves = Vec::new();
        string_leaves(&base, &mut Vec::new(), &mut leaves);
        let path = &leaves[pick.index(leaves.len())];
        let mut v = base;
        *at_mut(&mut v, path) = Value::String(text);
        if let Ok(c) = parse(&v) {
            assert_invariants(&c);
        }
    }

    /// Arbitrary bytes as a catalogue: an error, never a panic.
    #[test]
    fn arbitrary_text_never_panics(s in "\\PC{0,400}") {
        let _ = Catalogue::from_json(&s);
    }

    /// Replacing any number in a valid catalogue (`format`, a size, a port)
    /// with an edge value either is refused or keeps every invariant.
    #[test]
    fn number_mutations_never_weaken_the_catalogue(
        pick in any::<prop::sample::Index>(),
        n in prop_oneof![
            Just(0u64), Just(1), Just(80), Just(1023), Just(1024), Just(65535), Just(65536),
            Just(MAX_ARTIFACT_SIZE), Just(MAX_ARTIFACT_SIZE + 1), Just(u64::MAX), any::<u64>(),
        ],
    ) {
        let base = pinned();
        let mut leaves = Vec::new();
        number_leaves(&base, &mut Vec::new(), &mut leaves);
        let path = &leaves[pick.index(leaves.len())];
        let mut v = base;
        *at_mut(&mut v, path) = json!(n);
        if let Ok(c) = parse(&v) {
            assert_invariants(&c);
        }
    }

    /// Dropping any one field of a pinned artifact or port is refused,
    /// unless the field is optional (`executable`, `pin`, `digest`,
    /// `health`, `native`, `container`, `fingerprint`).
    #[test]
    fn dropping_a_required_field_is_refused(pick in any::<prop::sample::Index>()) {
        let base = pinned();
        let mut objects = Vec::new();
        object_keys(&base, &mut Vec::new(), &mut objects);
        let (path, key) = &objects[pick.index(objects.len())];
        let mut v = base;
        at_mut(&mut v, path).as_object_mut().unwrap().remove(key);
        let optional = ["executable", "pin", "digest", "health", "native", "container", "fingerprint"];
        let r = parse(&v);
        if !optional.contains(&key.as_str()) {
            prop_assert!(r.is_err(), "dropping {key} was accepted");
        } else if let Ok(c) = r {
            assert_invariants(&c);
        }
    }
}

/// Every number-valued leaf of a JSON value, by path.
fn number_leaves(v: &Value, path: &mut Vec<PathStep>, out: &mut Vec<Vec<PathStep>>) {
    match v {
        Value::Number(_) => out.push(path.clone()),
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(PathStep::Index(i));
                number_leaves(x, path, out);
                path.pop();
            }
        }
        Value::Object(o) => {
            for (k, x) in o {
                path.push(PathStep::Key(k.clone()));
                number_leaves(x, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

fn object_keys(v: &Value, path: &mut Vec<PathStep>, out: &mut Vec<(Vec<PathStep>, String)>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                out.push((path.clone(), k.clone()));
                path.push(PathStep::Key(k.clone()));
                object_keys(x, path, out);
                path.pop();
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(PathStep::Index(i));
                object_keys(x, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

/// What every accepted catalogue guarantees.
fn assert_invariants(c: &Catalogue) {
    for e in c.entries() {
        for h in &e.hosts {
            assert!(DOWNLOAD_HOSTS.contains(&h.as_str()), "{h}");
        }
        if e.needs(Need::ChallengeSolver) {
            assert!(e.opt_in);
        }
        if let Some(n) = &e.native {
            for a in &n.artifacts {
                assert!(SUPPORTED.contains(&(a.platform, a.arch)));
                if let Some(x) = &a.executable {
                    assert!(
                        x.components()
                            .all(|p| p != ".." && p != "." && !p.is_empty())
                    );
                    assert!(!x.as_str().starts_with('/'));
                }
                if let Ok(p) = a.pin() {
                    assert!(p.url.as_str().starts_with("https://"));
                    assert!(e.allows_host(p.url.host()));
                    assert!(p.size > 0 && p.size <= MAX_ARTIFACT_SIZE);
                }
            }
        }
        if let Ok((_, image)) = e.renderable() {
            assert!(REGISTRIES.contains(&image.registry()));
            assert!(!image.repository().contains([':', '@']));
        }
        for p in &e.ports {
            assert!(p.number.get() >= 1024);
            assert!(p.exposure == Exposure::Internal || e.needs(Need::DownloadClient));
        }
        if let Some(h) = &e.health {
            assert!(h.path.as_str().starts_with('/'));
            assert!(!h.path.as_str().contains(".."));
        }
    }
}
