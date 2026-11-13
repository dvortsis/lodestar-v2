//! CMP-7: generated compose services and Unraid templates.
//!
//! - Golden files per companion, with and without gluetun
//!   (`tests/golden/`), and the packaging examples (`packaging/companions/`),
//!   both byte-identical to what the code writes. Regenerate with
//!   `UPDATE_GOLDEN=1 cargo test -p lodestar-companions --test templates`.
//! - A property test: no input value adds a YAML key, document, anchor or
//!   alias, or an XML element or attribute.
//! - Refusals: bad paths, ports and combinations.

use std::path::{Path, PathBuf};

use lodestar_companions::catalogue::{Catalogue, NotAvailable, PortName};
use lodestar_companions::compose::{
    Bind, DataFolder, DataFolders, Paths, Publish, Route, ServiceInput, TemplateError, compose,
};
use lodestar_companions::inputs::{
    CompanionId, ContainerName, HostPath, InputError, NetworkName, Pgid, PortNumber, Puid,
};
use lodestar_companions::unraid::{UnraidTemplate, unraid};
use proptest::prelude::*;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde_json::{Value, json};

const BUILTIN: &str = include_str!("../data/catalogue.json");

/// The compiled-in catalogue with every unpinned image given `digest`,
/// plus a FlareSolverr entry (CMP-12 adds the real one).
fn catalogue(digest: &str) -> Catalogue {
    let mut v: Value = serde_json::from_str(BUILTIN).expect("catalogue json");
    let entries = v["entries"].as_array_mut().expect("entries");
    if !entries.iter().any(|e| e["id"] == "flaresolverr") {
        entries.push(json!({
            "id": "flaresolverr", "name": "FlareSolverr",
            "licence": {"spdx": "MIT", "url": "https://github.com/FlareSolverr/FlareSolverr/blob/master/LICENSE"},
            "homepage": "https://github.com/FlareSolverr/FlareSolverr",
            "needed_for": ["challenge_solver"], "opt_in": true, "hosts": [], "native": null,
            "container": {"image": "ghcr.io/flaresolverr/flaresolverr", "digest": null, "name": "flaresolverr"},
            "ports": [{"name": "api", "port": 8191, "proto": "tcp", "exposure": "internal"}],
            "health": null, "templates": ["compose_service", "unraid_template"]
        }));
    }
    for e in entries.iter_mut() {
        if e["container"]["digest"].is_null() {
            e["container"]["digest"] = json!(format!("sha256:{digest}"));
        }
    }
    Catalogue::from_json(&v.to_string()).expect("test catalogue validates")
}

fn golden_catalogue() -> Catalogue {
    catalogue(&"ab".repeat(32))
}

fn id(s: &str) -> CompanionId {
    CompanionId::parse(s).expect("id")
}

fn path(s: &str) -> HostPath {
    HostPath::parse(s).expect("host path")
}

fn name(s: &str) -> ContainerName {
    ContainerName::parse(s).expect("container name")
}

fn port(s: &str) -> PortName {
    PortName::parse(s).expect("port name")
}

fn num(n: u16) -> PortNumber {
    PortNumber::new(n).expect("port")
}

/// The inputs a companion needs, Unraid-style.
fn input(companion: &str, route: Route) -> ServiceInput {
    let (config, downloads, publish) = match companion {
        "qbittorrent" => (
            Some(path("/mnt/user/appdata/qbittorrent")),
            Some(path("/mnt/user/downloads")),
            vec![
                Publish {
                    port: port("webui"),
                    bind: Bind::Lan,
                    host_port: num(8080),
                },
                Publish {
                    port: port("p2p"),
                    bind: Bind::Lan,
                    host_port: num(6882),
                },
                Publish {
                    port: port("p2p_udp"),
                    bind: Bind::Lan,
                    host_port: num(6882),
                },
            ],
        ),
        "prowlarr" => (
            Some(path("/mnt/user/appdata/prowlarr")),
            None,
            vec![Publish {
                port: port("webui"),
                bind: Bind::Loopback,
                host_port: num(9696),
            }],
        ),
        _ => (None, None, Vec::new()),
    };
    // A peer port can't be published through the VPN.
    let publish = publish
        .into_iter()
        .filter(|p| route == Route::Direct || !p.port.as_str().starts_with("p2p"))
        .collect();
    ServiceInput {
        puid: Puid::new(99).expect("puid"),
        pgid: Pgid::new(100).expect("pgid"),
        network: NetworkName::parse("lodestar").expect("network"),
        route,
        paths: Paths { config, downloads },
        publish,
        data: DataFolders::new(
            "/mnt/cache/appdata/lodestar",
            Some("/mnt/nvme/lodestar-hot"),
            None,
        )
        .expect("data folders"),
        taken: Vec::new(),
    }
}

fn update() -> bool {
    std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| v == "1")
}

/// Compares `actual` with the file, or rewrites it under `UPDATE_GOLDEN`.
fn golden(file: &Path, actual: &str) {
    if update() {
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(file, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(file)
        .unwrap_or_else(|e| panic!("{}: {e}; run with UPDATE_GOLDEN=1", file.display()));
    // Git may check text out with CRLF on Windows.
    assert_eq!(
        expected.replace("\r\n", "\n"),
        actual,
        "{} differs; run with UPDATE_GOLDEN=1 and review the diff",
        file.display()
    );
}

fn here(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Checks what every output must hold, whatever the inputs.
fn invariants(yaml: &str, xml: &[UnraidTemplate]) {
    for bad in [
        "docker.sock",
        "privileged\":",
        "\"pid\"",
        "\"host\"",
        "$",
        "/var/run",
        "\"ipc\"",
    ] {
        assert!(!yaml.contains(bad), "compose holds {bad}");
    }
    assert!(!yaml.contains(":latest"));
    for line in yaml.lines().filter(|l| l.contains("\"image\":")) {
        assert!(line.contains("@sha256:"), "{line}");
    }
    let caps = yaml.matches("\"cap_add\"").count();
    assert_eq!(
        caps,
        yaml.matches("qmcgaw/gluetun@").count(),
        "only gluetun gets a capability"
    );
    for t in xml {
        assert!(!t.xml.contains("docker.sock") && !t.xml.contains("--privileged"));
        assert!(t.xml.contains("<Privileged>false</Privileged>"));
        assert!(t.xml.contains("@sha256:"));
        // Unraid pastes ExtraParams into a shell line unescaped.
        let extra = t
            .xml
            .split("<ExtraParams>")
            .nth(1)
            .map_or("", |s| s.split('<').next().unwrap_or(""));
        assert!(
            extra
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b" -_=:./,".contains(&b)),
            "{extra}"
        );
    }
}

#[test]
fn golden_per_companion() {
    let cat = golden_catalogue();
    for companion in ["qbittorrent", "prowlarr", "flaresolverr", "gluetun"] {
        let mut routes = vec![(Route::Direct, "")];
        if companion != "gluetun" {
            routes.push((Route::Gluetun, "-gluetun"));
            routes.push((Route::ExistingGluetun(name("my-vpn")), "-existing"));
        }
        for (route, suffix) in routes {
            let mut inp = input(companion, route.clone());
            if matches!(route, Route::ExistingGluetun(_)) {
                inp.publish.clear();
            }
            let yaml = compose(&cat, &id(companion), &inp).expect("compose");
            let xml = unraid(&cat, &id(companion), &inp).expect("unraid");
            invariants(&yaml, &xml);
            golden(
                &here(&format!("tests/golden/compose/{companion}{suffix}.yml")),
                &yaml,
            );
            for t in &xml {
                golden(
                    &here(&format!(
                        "tests/golden/unraid/{companion}{suffix}/{}",
                        t.file_name
                    )),
                    &t.xml,
                );
            }
            assert_eq!(xml.len(), if route == Route::Gluetun { 2 } else { 1 });
            if route == Route::Gluetun {
                assert_eq!(xml[0].file_name, "my-gluetun.xml", "gluetun comes first");
            }
        }
    }
}

/// Names and paths that read as YAML 1.1 keywords or numbers, and paths
/// with spaces, stay strings.
#[test]
fn golden_yaml_keywords() {
    let cat = golden_catalogue();
    let mut inp = input("qbittorrent", Route::Direct);
    inp.network = NetworkName::parse("null").expect("network");
    inp.paths.config = Some(path("/mnt/yes/on/0x10"));
    inp.paths.downloads = Some(path("/mnt/user/My Downloads/1e3 true"));
    inp.publish.clear();
    let yaml = compose(&cat, &id("qbittorrent"), &inp).expect("compose");
    assert!(yaml.contains("\"source\": \"/mnt/user/My Downloads/1e3 true\""));
    assert!(yaml.contains("\"null\":"));
    golden(&here("tests/golden/compose/keywords.yml"), &yaml);
    let xml = unraid(&cat, &id("qbittorrent"), &inp).expect("unraid");
    golden(&here("tests/golden/unraid/keywords.xml"), &xml[0].xml);
}

/// `packaging/companions/` is exactly what the code writes from the
/// compiled-in catalogue. An image still awaiting its pin is written with
/// the digest `00…01`, which no registry serves (the catalogue refuses an
/// all-zero one as a placeholder), so an example copied
/// before CMP-13 pins it can't pull anything.
#[test]
fn packaging_snippets_match() {
    let cat = catalogue(&format!("{}1", "0".repeat(63)));
    let mut written = Vec::new();
    for companion in ["qbittorrent", "prowlarr", "gluetun"] {
        for route in [Route::Direct, Route::Gluetun] {
            if companion == "gluetun" && route == Route::Gluetun {
                continue;
            }
            let suffix = if route == Route::Gluetun {
                "-gluetun"
            } else {
                ""
            };
            let mut compose_in = input(companion, route.clone());
            compose_in.puid = Puid::new(1000).expect("puid");
            compose_in.pgid = Pgid::new(1000).expect("pgid");
            compose_in.data = DataFolders::new("/srv/lodestar", None, None).expect("data");
            if let Some(c) = &mut compose_in.paths.config {
                *c = path(&format!("/srv/companions/{companion}"));
            }
            if compose_in.paths.downloads.is_some() {
                compose_in.paths.downloads = Some(path("/srv/downloads"));
            }
            // The web UI on 127.0.0.1 only; no peer port.
            compose_in.publish.retain(|p| p.port.as_str() == "webui");
            for p in &mut compose_in.publish {
                p.bind = Bind::Loopback;
            }
            let yaml = compose(&cat, &id(companion), &compose_in).expect("compose");
            written.push(format!("compose/{companion}{suffix}.yml"));
            golden(
                &here(&format!(
                    "../../packaging/companions/compose/{companion}{suffix}.yml"
                )),
                &yaml,
            );
            // The web UI stays off your network in the shipped examples
            // (§18.10); only the direct qBittorrent example publishes its
            // peer port on the LAN.
            let mut unraid_in = input(companion, route);
            for p in &mut unraid_in.publish {
                if p.port.as_str() == "webui" {
                    p.bind = Bind::Loopback;
                }
            }
            for t in unraid(&cat, &id(companion), &unraid_in).expect("unraid") {
                assert!(
                    !t.xml.contains("Web UI port"),
                    "web UI published on the LAN"
                );
                written.push(format!("unraid/{companion}{suffix}/{}", t.file_name));
                golden(
                    &here(&format!(
                        "../../packaging/companions/unraid/{companion}{suffix}/{}",
                        t.file_name
                    )),
                    &t.xml,
                );
            }
        }
    }
    // No file left over from an earlier layout.
    let root = here("../../packaging/companions");
    let mut found = Vec::new();
    for dir in ["compose", "unraid"] {
        list(&root.join(dir), &root, &mut found);
    }
    found.sort();
    written.sort();
    assert_eq!(
        found, written,
        "packaging/companions holds files the code doesn't write"
    );
}

fn list(dir: &Path, root: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).expect("read dir") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            list(&p, root, out);
        } else {
            let rel = p.strip_prefix(root).expect("under root");
            let parts: Vec<_> = rel
                .iter()
                .map(|c| c.to_string_lossy().into_owned())
                .collect();
            out.push(parts.join("/"));
        }
    }
}

#[test]
fn unraid_fields_and_link() {
    let cat = golden_catalogue();
    let t = unraid(&cat, &id("gluetun"), &input("gluetun", Route::Direct)).expect("unraid");
    assert_eq!(t[0].file_name, "my-gluetun.xml");
    assert_eq!(
        t[0].add_container_path(),
        "/Docker/AddContainer?xmlTemplate=user:/boot/config/plugins/dockerMan/templates-user/my-gluetun.xml"
    );
    let get = |k: &str| {
        t[0].fields
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(
        get("WIREGUARD_PRIVATE_KEY"),
        Some(""),
        "VPN keys stay empty"
    );
    assert_eq!(
        get("Extra Parameters"),
        Some("--cap-add=NET_ADMIN --device=/dev/net/tun")
    );
    assert!(
        t[0].xml.contains(
            "Name=\"WIREGUARD_PRIVATE_KEY\" Target=\"WIREGUARD_PRIVATE_KEY\" Default=\"\""
        )
    );
}

#[test]
fn gluetun_placeholders_never_hold_a_key() {
    let cat = golden_catalogue();
    let yaml = compose(
        &cat,
        &id("qbittorrent"),
        &input("qbittorrent", Route::Gluetun),
    )
    .expect("compose");
    assert!(yaml.contains("\"WIREGUARD_PRIVATE_KEY\": \"FILL_IN_YOURSELF\""));
    assert!(yaml.contains("Lodestar never asks for"));
    assert!(yaml.contains("\"network_mode\": \"service:gluetun\""));
    // Its ports are on gluetun, never on the routed companion.
    let qbit = yaml
        .split("  \"qbittorrent\":")
        .nth(1)
        .expect("qbittorrent service");
    assert!(!qbit.contains("\"ports\""));
}

// ─── Refusals ───────────────────────────────────────────────────────────

fn refuse(companion: &str, f: impl FnOnce(&mut ServiceInput), want: TemplateError) {
    let cat = golden_catalogue();
    let mut inp = input(companion, Route::Direct);
    f(&mut inp);
    assert_eq!(compose(&cat, &id(companion), &inp), Err(want));
    assert_eq!(unraid(&cat, &id(companion), &inp).err(), Some(want));
}

#[test]
fn refuses_bad_host_paths_and_ports() {
    assert_eq!(HostPath::parse("mnt/user/x"), Err(InputError::NotAbsolute));
    assert_eq!(
        HostPath::parse("/mnt/user/../etc"),
        Err(InputError::Component)
    );
    assert_eq!(HostPath::parse("/mnt/user/./x"), Err(InputError::Component));
    assert_eq!(PortNumber::parse("80"), Err(InputError::Range));
    assert_eq!(PortNumber::parse("65536"), Err(InputError::Range));
    assert_eq!(PortNumber::parse("0"), Err(InputError::Range));
    assert_eq!(DataFolder::parse("data"), Err(InputError::NotAbsolute));
    assert_eq!(DataFolder::parse("/srv/../x"), Err(InputError::Component));
    assert!(matches!(
        DataFolders::new("relative", None, None),
        Err(TemplateError::BadDataFolder(InputError::NotAbsolute))
    ));
}

#[test]
fn refuses_overlaps_with_lodestar_data() {
    let inside = |p: &'static str| move |i: &mut ServiceInput| i.paths.config = Some(path(p));
    refuse(
        "prowlarr",
        inside("/mnt/nvme/lodestar-hot/x"),
        TemplateError::DataFolder,
    );
    refuse(
        "prowlarr",
        |i| i.data = DataFolders::new("/mnt/user/appdata/prowlarr/db", None, None).expect("data"),
        TemplateError::DataFolder,
    );
    // Case doesn't hide it.
    refuse(
        "prowlarr",
        |i| {
            i.data =
                DataFolders::none().with(DataFolder::parse("/MNT/User/Stuff").expect("folder"));
            i.paths.config = Some(path("/mnt/user/stuff/prowlarr"));
        },
        TemplateError::DataFolder,
    );
    refuse(
        "qbittorrent",
        inside("/mnt/user/appdata/prowlarr"),
        TemplateError::OtherCompanion,
    );
    refuse(
        "qbittorrent",
        inside("/mnt/user/AppData/Prowlarr/x"),
        TemplateError::OtherCompanion,
    );
    refuse(
        "qbittorrent",
        inside("/mnt/c/Users/me/.ssh"),
        TemplateError::DotFolder,
    );
    // One Unraid share through any pool or disk is the same folder.
    for alias in [
        "/mnt/user/lodestar/x",
        "/mnt/user0/lodestar",
        "/mnt/disk1/lodestar/db",
    ] {
        refuse(
            "prowlarr",
            move |i| {
                i.data = DataFolders::new("/mnt/cache/lodestar", Some(""), Some("")).expect("data");
                i.paths.config = Some(path(alias));
            },
            TemplateError::DataFolder,
        );
    }
    refuse(
        "prowlarr",
        inside("/mnt/user/lodestar-hot/x"),
        TemplateError::DataFolder,
    );
    // Another companion's folder, either way round, through aliases too.
    refuse(
        "qbittorrent",
        |i| {
            i.taken = vec![path("/srv/companions/prowlarr")];
            i.paths.config = Some(path("/srv/companions"));
        },
        TemplateError::OtherCompanion,
    );
    refuse(
        "prowlarr",
        |i| {
            i.taken = vec![path("/mnt/cache/stuff/qbittorrent")];
            i.paths.config = Some(path("/mnt/user/stuff"));
        },
        TemplateError::OtherCompanion,
    );
}

/// The running instance's folder is compared after resolving links.
#[cfg(target_os = "linux")]
#[test]
fn refuses_the_running_data_folder_through_a_link() {
    let dir = tempfile::Builder::new()
        .prefix("cmp7")
        .tempdir()
        .expect("tempdir");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("mkdir");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let real = std::fs::canonicalize(&real).expect("canonical");
    let folder = DataFolder::from_running(&link).expect("resolves");
    let sub = real.join("sub");
    refuse(
        "prowlarr",
        move |i| {
            i.data = DataFolders::none().with(folder);
            i.paths.config = Some(path(sub.to_str().expect("utf8")));
        },
        TemplateError::DataFolder,
    );
    assert_eq!(
        DataFolder::from_running(&dir.path().join("missing")),
        Err(TemplateError::DataFolderUnreadable)
    );
}

#[test]
fn refuses_wrong_shapes() {
    refuse(
        "qbittorrent",
        |i| i.paths.downloads = None,
        TemplateError::MissingPath(lodestar_companions::compose::MountRole::Downloads),
    );
    refuse(
        "prowlarr",
        |i| i.paths.downloads = Some(path("/mnt/user/downloads")),
        TemplateError::UnexpectedPath(lodestar_companions::compose::MountRole::Downloads),
    );
    refuse(
        "prowlarr",
        |i| i.publish[0].port = port("p2p"),
        TemplateError::UnknownPort,
    );
    refuse(
        "prowlarr",
        |i| i.publish.push(i.publish[0].clone()),
        TemplateError::DuplicatePort,
    );
    refuse(
        "qbittorrent",
        |i| i.publish[1].host_port = num(8080),
        TemplateError::DuplicatePort,
    );
    refuse(
        "gluetun",
        |i| i.route = Route::Gluetun,
        TemplateError::GluetunSelf,
    );
    refuse(
        "qbittorrent",
        |i| i.route = Route::Gluetun,
        TemplateError::P2pThroughVpn,
    );
    refuse(
        "qbittorrent",
        |i| i.publish[2].bind = Bind::Loopback,
        TemplateError::P2pLoopback,
    );
    refuse(
        "qbittorrent",
        |i| i.publish[0].host_port = num(8081),
        TemplateError::PortRemap,
    );
    refuse(
        "qbittorrent",
        |i| i.publish[2].host_port = num(6883),
        TemplateError::PortRemap,
    );
    refuse(
        "gluetun",
        |i| {
            i.publish = vec![Publish {
                port: port("control"),
                bind: Bind::Lan,
                host_port: num(8000),
            }]
        },
        TemplateError::LanRefused,
    );
    refuse(
        "prowlarr",
        |i| i.route = Route::ExistingGluetun(name("my-vpn")),
        TemplateError::PublishOnExisting,
    );
    refuse(
        "prowlarr",
        |i| {
            i.publish.clear();
            i.route = Route::ExistingGluetun(name("qbittorrent"));
        },
        TemplateError::ExistingName,
    );
    let cat = golden_catalogue();
    assert_eq!(
        compose(&cat, &id("nzbget"), &input("x", Route::Direct)),
        Err(TemplateError::UnknownCompanion)
    );
}

#[test]
fn unpinned_images_are_refused() {
    let cat = lodestar_companions::builtin().expect("builtin");
    let Some(entry) = cat.get("qbittorrent") else {
        panic!("no qbittorrent")
    };
    if entry.renderable().is_ok() {
        return; // pinned by CMP-13; nothing to refuse.
    }
    let err = compose(
        cat,
        &id("qbittorrent"),
        &input("qbittorrent", Route::Direct),
    );
    assert_eq!(
        err,
        Err(TemplateError::NotAvailable(NotAvailable::AwaitingPin))
    );
    assert_eq!(err.err().map(TemplateError::code), Some("awaiting_pin"));
}

// ─── Injection ─────────────────────────────────────────────────────────

/// The YAML with every JSON string replaced by `S`, after checking each
/// one parses as a JSON string.
fn yaml_skeleton(yaml: &str) -> String {
    let mut out = String::new();
    for line in yaml.lines() {
        let mut start = None;
        let mut escaped = false;
        for (i, c) in line.char_indices() {
            match start {
                None if c == '#' => {
                    out.push_str(&line[i..]);
                    break;
                }
                None if c == '"' => start = Some(i),
                None => out.push(c),
                Some(_) if escaped => escaped = false,
                Some(_) if c == '\\' => escaped = true,
                Some(s) if c == '"' => {
                    serde_json::from_str::<String>(&line[s..=i]).expect("a JSON string");
                    out.push('S');
                    start = None;
                }
                Some(_) => {}
            }
        }
        assert!(start.is_none(), "unterminated string: {line}");
        out.push('\n');
    }
    out
}

/// Element names and attribute names, in order; text and values dropped.
fn xml_skeleton(xml: &str) -> String {
    let mut r = Reader::from_str(xml);
    let mut out = String::new();
    loop {
        match r.read_event().expect("well-formed XML") {
            Event::Start(e) | Event::Empty(e) => {
                out.push('<');
                out.push_str(e.name().into_inner());
                for a in e.attributes() {
                    let a = a.expect("attribute");
                    out.push(' ');
                    out.push_str(a.key.into_inner());
                }
                out.push('>');
            }
            Event::End(e) => {
                out.push_str("</");
                out.push_str(e.name().into_inner());
                out.push('>');
            }
            Event::Text(_) => {}
            // Only XML's predefined entities are ever written.
            Event::GeneralRef(g) => {
                assert!(
                    ["amp", "lt", "gt", "quot", "apos"].contains(&&*g.xml10_content()),
                    "{g:?}"
                );
            }
            Event::Eof => break,
            Event::Decl(_) => out.push_str("<?xml?>"),
            other => panic!("unexpected XML event {other:?}"),
        }
    }
    out
}

fn skeletons(inp: &ServiceInput) -> Option<(String, Vec<String>)> {
    let cat = golden_catalogue();
    let yaml = compose(&cat, &id("qbittorrent"), inp).ok()?;
    let xml = unraid(&cat, &id("qbittorrent"), inp).expect("unraid accepts what compose does");
    invariants(&yaml, &xml);
    assert!(
        !yaml
            .lines()
            .any(|l| l.starts_with("---") || l.starts_with("..."))
    );
    Some((
        yaml_skeleton(&yaml),
        xml.iter().map(|t| xml_skeleton(&t.xml)).collect(),
    ))
}

/// Text with every character that matters to YAML or XML.
fn hostile() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just("\n".to_owned()),
            Just(": ".to_owned()),
            Just("---".to_owned()),
            Just("&a".to_owned()),
            Just("*a".to_owned()),
            Just("<x>".to_owned()),
            Just("\"".to_owned()),
            Just("'".to_owned()),
            Just("\0".to_owned()),
            Just("#".to_owned()),
            Just(" ".to_owned()),
            Just("/".to_owned()),
            Just("!!str".to_owned()),
            Just("{a}".to_owned()),
            Just("$X".to_owned()),
            Just("..".to_owned()),
            Just("\\".to_owned()),
            Just("&amp;".to_owned()),
            "[a-z0-9._-]{1,6}",
        ],
        1..8,
    )
    .prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn no_input_changes_the_structure(
        text in hostile(),
        slot in 0usize..7,
        baseline in 0usize..3,
    ) {
        let base = match baseline {
            0 => input("qbittorrent", Route::Direct),
            1 => input("qbittorrent", Route::Gluetun),
            _ => {
                let mut b = input("qbittorrent", Route::ExistingGluetun(name("my-vpn")));
                b.publish.clear();
                b
            }
        };
        let (y0, x0) = skeletons(&base).expect("baseline renders");
        let mut inp = base.clone();
        let existing = matches!(base.route, Route::ExistingGluetun(_));
        let accepted = match slot {
            // The name of a gluetun you already run.
            6 if existing => ContainerName::parse(&text)
                .map(|n| inp.route = Route::ExistingGluetun(n))
                .is_ok(),
            6 => false,
            0 => HostPath::parse(&format!("/mnt/user/{text}")).map(|p| inp.paths.config = Some(p)).is_ok(),
            1 => HostPath::parse(&format!("/mnt/user/{text}")).map(|p| inp.paths.downloads = Some(p)).is_ok(),
            2 => NetworkName::parse(&text).map(|n| inp.network = n).is_ok(),
            3 => Puid::parse(&text).map(|p| inp.puid = p).is_ok(),
            4 => Pgid::parse(&text).map(|p| inp.pgid = p).is_ok(),
            _ => DataFolder::parse(&format!("/srv/{text}")).map(|d| inp.data = DataFolders::none().with(d)).is_ok(),
        };
        if accepted && let Some((y, x)) = skeletons(&inp) {
            prop_assert_eq!(y, y0);
            prop_assert_eq!(x, x0);
        }
    }
}

// ─── gluetun's post-rules (X-gluetun-templates) ─────────────────────────

/// The crate's embedded copy is `packaging/companions/gluetun-post-rules.txt`
/// byte for byte: the file users download, pinned by SHA-256 in the docs.
#[test]
fn embedded_post_rules_are_the_packaging_file() {
    let shipped =
        std::fs::read_to_string(here("../../packaging/companions/gluetun-post-rules.txt"))
            .expect("packaging rules file");
    assert_eq!(lodestar_companions::compose::GLUETUN_POST_RULES, shipped);
}

/// The interfaces a rules text names (`-o X` and `! -o X`), from its rule lines.
fn rule_interfaces(rules: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in rules
        .lines()
        .filter(|l| l.starts_with("iptables ") || l.starts_with("ip6tables "))
    {
        let words: Vec<&str> = line.split_whitespace().collect();
        for w in words.windows(2) {
            if w[0] == "-o" {
                out.push(w[1].to_owned());
            }
        }
    }
    out
}

/// The literal block under `"content": |` in a compose document, unindented.
fn config_content(yaml: &str) -> Option<String> {
    let mut lines = yaml
        .lines()
        .skip_while(|l| !l.trim_end().ends_with("\"content\": |"));
    let head = lines.next()?;
    let pad = head.len() - head.trim_start().len() + 2;
    let mut out = String::new();
    for l in lines {
        if !l.is_empty() && !l.starts_with(&" ".repeat(pad)) {
            break;
        }
        out.push_str(l.get(pad..).unwrap_or(""));
        out.push('\n');
    }
    Some(out)
}

/// Every template with gluetun carries the rules (compose: inline, exactly
/// the embedded text; Unraid: a read-only Path at gluetun's post-rules
/// location), and sets VPN_INTERFACE to the one interface the rules name.
#[test]
fn every_gluetun_template_carries_the_rules_for_its_interface() {
    use lodestar_companions::compose::{GLUETUN_INTERFACE, GLUETUN_POST_RULES};
    let names = rule_interfaces(GLUETUN_POST_RULES);
    assert_eq!(names.len(), 6, "three rules per family name the tunnel");
    assert!(names.iter().all(|n| n == GLUETUN_INTERFACE), "{names:?}");
    let cat = golden_catalogue();
    for (companion, route) in [
        ("gluetun", Route::Direct),
        ("qbittorrent", Route::Gluetun),
        ("prowlarr", Route::Gluetun),
        ("flaresolverr", Route::Gluetun),
    ] {
        let inp = input(companion, route);
        let yaml = compose(&cat, &id(companion), &inp).expect("compose");
        assert_eq!(
            config_content(&yaml).as_deref(),
            Some(GLUETUN_POST_RULES),
            "{companion}"
        );
        assert_eq!(
            yaml.matches(&format!("\"VPN_INTERFACE\": \"{GLUETUN_INTERFACE}\""))
                .count(),
            1,
            "{companion}"
        );
        assert!(
            yaml.contains("\"target\": \"/iptables/post-rules.txt\""),
            "{companion}"
        );
        let xml = unraid(&cat, &id(companion), &inp).expect("unraid");
        let g = &xml[0];
        assert_eq!(g.file_name, "my-gluetun.xml");
        assert!(g.xml.contains(
            "<Config Name=\"Post-rules\" Target=\"/iptables/post-rules.txt\" Default=\"/mnt/cache/appdata/gluetun/post-rules.txt\" Mode=\"ro\""
        ));
        assert!(
            g.xml.contains(&format!(
                "<Config Name=\"VPN_INTERFACE\" Target=\"VPN_INTERFACE\" Default=\"{GLUETUN_INTERFACE}\""
            ))
        );
        assert!(!g.xml.contains("blocks all traffic"), "the old overclaim");
        assert!(
            g.xml
                .contains("save Lodestar&apos;s post-rules file at the Post-rules path"),
            "gluetun's Overview says to save the file first"
        );
        assert!(
            g.xml.contains(&format!(
                "Keep {GLUETUN_INTERFACE}: Lodestar&apos;s post-rules name this tunnel"
            )),
            "VPN_INTERFACE's help names the value the rules need"
        );
        assert!(g.xml.contains("and check it, as the guide shows"));
        if companion != "gluetun" {
            // The routed companion is told to start after gluetun's firewall.
            assert!(xml[1].xml.contains("put gluetun above this container"));
        }
    }
    // Without gluetun there are no rules.
    let yaml = compose(&cat, &id("prowlarr"), &input("prowlarr", Route::Direct)).expect("compose");
    assert!(!yaml.contains("\"configs\""));
    let mut existing = input("prowlarr", Route::ExistingGluetun(name("my-vpn")));
    existing.publish.clear();
    let yaml = compose(&cat, &id("prowlarr"), &existing).expect("compose");
    assert!(
        !yaml.contains("\"configs\""),
        "an existing gluetun is yours to configure"
    );
    // Its Unraid template names that container in the start-order note.
    let xml = unraid(&cat, &id("prowlarr"), &existing).expect("unraid");
    assert!(xml[0].xml.contains("put my-vpn above this container"));
}
