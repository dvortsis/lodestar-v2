//! `bitmagnet_check` against real `SQL_ASCII` databases built from the
//! fixtures. The database tests run when `LODESTAR_TEST_POSTGRES` points at
//! a server the test may create databases on (CI provides one); they're
//! skipped otherwise. The usage tests always run.

use std::process::{Command, Output};
use std::str::FromStr;

use tokio_postgres::config::Host;
use tokio_postgres::{Client, Config, NoTls};

const SCHEMA: &str = include_str!("fixtures/bitmagnet_check/schema.sql");
const DATA: &str = include_str!("fixtures/bitmagnet_check/data.sql");

/// SHA-256 over the sampled values in key order, each as `0x01 ||
/// sha256(raw bytes)`, computed independently of the checker from the
/// bytes in `data.sql`.
const NAME_SHA256: &str = "ff3df48934bacb642c20e5454fd9fc1fed21c8ceca10b7ad1afcfa80d80a32f7";
const PATH_SHA256: &str = "38b976d8c55eb9676904b15bea1e64ebcb550a88ed391504db15a4de38a399b9";

struct Server {
    host: String,
    port: u16,
    user: String,
    password: Option<Vec<u8>>,
    base: String,
}

fn server() -> Option<Server> {
    let Ok(base) = std::env::var("LODESTAR_TEST_POSTGRES") else {
        eprintln!("skipped: set LODESTAR_TEST_POSTGRES to run");
        return None;
    };
    let cfg = Config::from_str(&base).expect("LODESTAR_TEST_POSTGRES is a conninfo string");
    let host = match cfg.get_hosts().first() {
        Some(Host::Tcp(h)) => h.clone(),
        #[cfg(unix)]
        Some(Host::Unix(p)) => p.to_string_lossy().into_owned(),
        None => "localhost".into(),
    };
    Some(Server {
        host,
        port: cfg.get_ports().first().copied().unwrap_or(5432),
        user: cfg.get_user().unwrap_or("postgres").to_string(),
        password: cfg.get_password().map(<[u8]>::to_vec),
        base,
    })
}

async fn connect(conninfo: &str) -> Client {
    let (client, conn) = tokio_postgres::connect(conninfo, NoTls).await.unwrap();
    tokio::spawn(conn);
    client
}

/// A fresh database named for this process and `tag`, loaded with the
/// fixture schema and, if `data`, its rows.
async fn database(s: &Server, tag: &str, encoding: &str, data: bool) -> String {
    let name = format!("lodestar_bmcheck_{}_{tag}", std::process::id());
    let admin = connect(&s.base).await;
    // Separately: CREATE DATABASE can't share an implicit transaction.
    admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .await
        .unwrap();
    admin
        .batch_execute(&format!(
            "CREATE DATABASE {name} ENCODING '{encoding}' \
             LC_COLLATE 'C' LC_CTYPE 'C' TEMPLATE template0"
        ))
        .await
        .unwrap();
    let db = connect(&format!("{} dbname={name}", s.base)).await;
    db.batch_execute(SCHEMA).await.unwrap();
    if data {
        db.batch_execute(DATA).await.unwrap();
    }
    name
}

async fn drop_database(s: &Server, name: &str) {
    let admin = connect(&s.base).await;
    let _ = admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .await;
}

fn checker() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_bitmagnet_check"));
    for k in ["PGHOST", "PGPORT", "PGUSER", "PGDATABASE", "PGPASSWORD"] {
        c.env_remove(k);
    }
    c
}

fn run(s: &Server, dbname: &str, extra: &[&str]) -> Output {
    let mut c = checker();
    let port = s.port.to_string();
    c.args([
        "--host", &s.host, "--port", &port, "--user", &s.user, "--dbname", dbname,
    ]);
    c.args(extra);
    if let Some(p) = &s.password {
        c.env("PGPASSWORD", String::from_utf8(p.clone()).unwrap());
    }
    c.output().unwrap()
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checks_a_restored_sql_ascii_database() {
    let Some(s) = server() else { return };
    let db = database(&s, "ok", "SQL_ASCII", true).await;
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_str().unwrap();
    let out = run(&s, &db, &["--data-dir", data_dir, "--reserve", "1"]);
    let (json, summary) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{summary}\n{json}");
    for want in [
        r#""ok":true"#,
        r#""encoding":"SQL_ASCII","collate":"C","ctype":"C""#,
        r#""missing":[],"wrong_type":[],"missing_enum_labels":[],"extra_tables":["not_bitmagnet"],"extra_columns":[]"#,
        r#"{"name":"torrents","rows_estimate":"#,
        // Five names: ASCII and UTF-8 are valid UTF-8; Latin-1, GBK and the
        // invalid sequences aren't. Four contain bytes over 0x7f.
        &format!(
            r#"{{"table":"torrents","column":"name","rows":5,"nulls":0,"utf8_valid":2,"non_ascii":4,"bytes":40,"sha256":"{NAME_SHA256}"}}"#
        ),
        // Four paths: two Latin-1, one GBK, one UTF-8.
        &format!(
            r#"{{"table":"torrent_files","column":"path","rows":4,"nulls":0,"utf8_valid":1,"non_ascii":4,"bytes":50,"sha256":"{PATH_SHA256}"}}"#
        ),
        r#""reserve_bytes":1}"#,
    ] {
        assert!(json.contains(want), "missing {want}\nin {json}");
    }
    for (table, n) in [("torrents", 5), ("torrent_files", 4), ("torrent_pieces", 1)] {
        assert!(
            json.contains(&format!(r#""{table}","rows_estimate":"#))
                && json.contains(&format!(r#","rows_exact":{n}}}"#)),
            "{table}: {json}"
        );
    }
    assert!(summary.contains("all checks passed"), "{summary}");

    // The same seed samples the same rows; a reserve above the free space fails.
    let again = run(
        &s,
        &db,
        &["--data-dir", data_dir, "--reserve", "1000000TiB"],
    );
    let (json2, summary2) = text(&again);
    assert_eq!(again.status.code(), Some(1), "{summary2}");
    assert!(json2.contains(NAME_SHA256) && json2.contains(PATH_SHA256));
    assert!(
        json2.contains(r#"{"name":"disk","status":"fail""#),
        "{json2}"
    );

    // A sample smaller than the table is bounded, and still full.
    let small = run(&s, &db, &["--sample", "1", "--counts", "estimate"]);
    let (json3, _) = text(&small);
    assert!(json3.contains(r#""column":"name","rows":1,"#), "{json3}");
    assert!(
        json3.contains(r#"{"name":"row_counts","status":"skipped""#),
        "{json3}"
    );
    drop_database(&s, &db).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn functions_the_database_defines_cant_forge_the_results() {
    let Some(s) = server() else { return };
    let db = database(&s, "hijack", "SQL_ASCII", true).await;
    let c = connect(&format!("{} dbname={db}", s.base)).await;
    // Better overload matches than pg_catalog's for the checker's calls:
    // with `public` on the search path, they'd win and forge everything.
    c.batch_execute(
        r#"CREATE FUNCTION public.convert_to(text, text) RETURNS bytea
             LANGUAGE sql AS $$ SELECT '\x5554463800'::bytea $$;
           CREATE FUNCTION public.sha256(bytea) RETURNS bytea
             LANGUAGE sql AS $$ SELECT '\x00'::bytea $$;
           CREATE FUNCTION public.length(bytea) RETURNS integer
             LANGUAGE sql AS $$ SELECT 0 $$;"#,
    )
    .await
    .unwrap();
    let out = run(&s, &db, &[]);
    let (json, summary) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{summary}\n{json}");
    assert!(json.contains(r#""encoding":"SQL_ASCII""#), "{json}");
    assert!(
        json.contains(NAME_SHA256) && json.contains(PATH_SHA256),
        "{json}"
    );
    assert!(json.contains(r#""bytes":40,"#), "{json}");
    drop_database(&s, &db).await;
}

/// The runbook's own `reader-role.sql`, through psql: the role can't
/// write even after turning its read-only default off, the checker passes
/// as that role, and a row-level security policy can't hide rows from it.
/// Skipped where psql isn't installed. Roles are cluster-wide, so this is
/// the only test that creates `bitmagnet_reader`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reader_role_cant_write_and_rls_cant_hide_rows() {
    let Some(s) = server() else { return };
    if Command::new("psql").arg("--version").output().is_err() {
        eprintln!("skipped: needs psql on PATH");
        return;
    }
    let db = database(&s, "reader", "SQL_ASCII", true).await;
    let dir = tempfile::tempdir().unwrap();
    let pw_file = dir.path().join("reader_pw");
    let pw = "0123456789abcdef0123456789abcdef0123456789abcdef";
    std::fs::write(&pw_file, pw).unwrap();
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/operations/imports/bitmagnet-restore/reader-role.sql"
    );
    let port = s.port.to_string();
    let mut psql = Command::new("psql");
    psql.args([
        "-X", "-h", &s.host, "-p", &port, "-U", &s.user, "-d", &db, "-f", script,
    ])
    .env("READER_PW_FILE", &pw_file);
    if let Some(p) = &s.password {
        psql.env("PGPASSWORD", String::from_utf8(p.clone()).unwrap());
    }
    let out = psql.output().unwrap();
    let (stdout, stderr) = text(&out);
    assert!(
        stdout.contains("bitmagnet_reader is ready"),
        "{stdout}\n{stderr}"
    );

    let reader = connect(&format!(
        "{} dbname={db} user=bitmagnet_reader password={pw}",
        s.base
    ))
    .await;
    for sql in [
        "SET default_transaction_read_only = off; SELECT lo_from_bytea(0, '\\x41')",
        "BEGIN READ WRITE; SELECT lo_create(0)",
        "BEGIN READ WRITE; CREATE TEMP TABLE t (i int)",
        "BEGIN READ WRITE; CREATE TABLE public.t (i int)",
        "BEGIN READ WRITE; DELETE FROM public.torrent_pieces",
    ] {
        let e = reader.batch_execute(sql).await.unwrap_err();
        let code = e.code().map(|c| c.code().to_string());
        assert_eq!(code.as_deref(), Some("42501"), "{sql}: {e:?}");
        let _ = reader.batch_execute("ROLLBACK").await;
    }

    // The checker as the importer's role: least privilege is enough.
    let mut c = checker();
    c.args([
        "--host",
        &s.host,
        "--port",
        &port,
        "--user",
        "bitmagnet_reader",
        "--dbname",
        &db,
    ])
    .arg("--password-file")
    .arg(&pw_file);
    let out = c.output().unwrap();
    let (json, summary) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{summary}\n{json}");
    assert!(
        json.contains(NAME_SHA256) && json.contains(PATH_SHA256),
        "{json}"
    );

    // A policy showing one torrent of five: the counts and the sample fail
    // rather than report one.
    let admin = connect(&format!("{} dbname={db}", s.base)).await;
    admin
        .batch_execute(
            "ALTER TABLE torrents ENABLE ROW LEVEL SECURITY;
             CREATE POLICY one ON torrents FOR SELECT USING (substr(info_hash, 1, 1) = '\\x01'::bytea)",
        )
        .await
        .unwrap();
    let out = c.output().unwrap();
    let (json, summary) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{summary}\n{json}");
    assert!(
        json.contains(r#"{"name":"row_counts","status":"fail""#),
        "{json}"
    );
    assert!(
        json.contains(r#"{"name":"samples","status":"fail""#),
        "{json}"
    );
    let torrents = json.split(r#"{"name":"torrents","#).nth(1).unwrap();
    let torrents = &torrents[..torrents.find('}').unwrap()];
    assert!(torrents.ends_with(r#""rows_exact":null"#), "{torrents}");

    drop(reader);
    drop_database(&s, &db).await;
    let _ = admin;
    let _ = connect(&s.base)
        .await
        .batch_execute("DROP ROLE IF EXISTS bitmagnet_reader")
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_database_fails_even_on_estimates() {
    let Some(s) = server() else { return };
    let db = database(&s, "empty", "SQL_ASCII", false).await;
    let out = run(&s, &db, &["--counts", "estimate"]);
    let (json, summary) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{summary}\n{json}");
    assert!(
        json.contains(r#""detail":"no torrents to sample""#),
        "{json}"
    );
    drop_database(&s, &db).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_or_retyped_column_fails_the_schema_check() {
    let Some(s) = server() else { return };
    let db = database(&s, "schema", "SQL_ASCII", true).await;
    let c = connect(&format!("{} dbname={db}", s.base)).await;
    c.batch_execute(
        "ALTER TABLE torrents DROP COLUMN files_count;
         ALTER TABLE torrent_files ALTER COLUMN size TYPE integer;
         ALTER TABLE torrents ADD COLUMN surprise text;",
    )
    .await
    .unwrap();
    let out = run(&s, &db, &[]);
    let (json, summary) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{summary}");
    assert!(
        json.contains(r#""missing":["torrents.files_count"]"#),
        "{json}"
    );
    assert!(
        json.contains(
            r#""wrong_type":[{"column":"torrent_files.size","expected":"int8","found":"int4"}]"#
        ),
        "{json}"
    );
    assert!(
        json.contains(r#""extra_columns":["torrents.surprise"]"#),
        "{json}"
    );
    assert!(
        json.contains(r#"{"name":"schema","status":"fail""#),
        "{json}"
    );
    assert!(
        json.contains(r#"{"name":"samples","status":"skipped""#),
        "{json}"
    );
    assert!(
        summary.contains("missing torrents.files_count"),
        "{summary}"
    );
    drop_database(&s, &db).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transcoded_database_fails_the_encoding_check() {
    let Some(s) = server() else { return };
    let db = database(&s, "utf8", "UTF8", false).await;
    let c = connect(&format!("{} dbname={db}", s.base)).await;
    c.batch_execute(
        "INSERT INTO torrents (info_hash, name, size, private, created_at, updated_at)
         VALUES (decode(repeat('01', 20), 'hex'), 'x', 1, false, now(), now())",
    )
    .await
    .unwrap();
    let out = run(&s, &db, &[]);
    let (json, summary) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{summary}");
    assert!(
        json.contains(r#"{"name":"encoding","status":"fail""#),
        "{json}"
    );
    assert!(json.contains(r#""encoding":"UTF8""#), "{json}");
    // A data directory declares its own encoding: --encoding names it.
    let declared = run(&s, &db, &["--encoding", "UTF8"]);
    let (json, summary) = text(&declared);
    assert_eq!(declared.status.code(), Some(0), "{summary}\n{json}");
    drop_database(&s, &db).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_password_never_reaches_argv_or_output() {
    let Some(s) = server() else { return };
    let db = database(&s, "secret", "SQL_ASCII", true).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pw");
    // The file wins over PGPASSWORD, so a server that checks passwords would
    // see this one. CI's trusts every login, so both only need to not leak.
    let in_file = "file-secret-7f3a9c";
    let in_env = "env-secret-2b8d41";
    std::fs::write(&file, format!("{in_file}\n")).unwrap();
    let port = s.port.to_string();
    for dbname in [db.as_str(), "no_such_database_for_bmcheck"] {
        let mut c = checker();
        c.args([
            "--host", &s.host, "--port", &port, "--user", &s.user, "--dbname", dbname,
        ])
        .arg("--password-file")
        .arg(&file)
        .env("PGPASSWORD", in_env);
        let argv: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !argv
                .iter()
                .any(|a| a.contains(in_file) || a.contains(in_env))
        );
        let out = c.output().unwrap();
        let (stdout, stderr) = text(&out);
        let all = format!("{stdout}{stderr}");
        assert!(!all.contains(in_file) && !all.contains(in_env), "{all}");
        if dbname == db {
            // Only reached when the server accepts the login.
            if s.password.is_none() {
                assert_eq!(out.status.code(), Some(0), "{all}");
            }
        } else {
            assert_eq!(out.status.code(), Some(2), "{all}");
        }
    }
    drop_database(&s, &db).await;
}

#[test]
fn a_password_on_the_command_line_is_refused_unechoed() {
    for args in [
        &["--host", "/run/pg", "--password", "argv-secret-91c2"][..],
        &["--host", "/run/pg", "--password=argv-secret-91c2"],
    ] {
        let out = checker().args(args).output().unwrap();
        let (stdout, stderr) = text(&out);
        assert_eq!(out.status.code(), Some(2));
        assert!(stderr.contains("--password-file"), "{stderr}");
        assert!(!format!("{stdout}{stderr}").contains("argv-secret-91c2"));
    }
}

#[test]
fn only_a_socket_or_loopback_is_reached() {
    for host in ["192.0.2.10", "db.example.invalid", "0.0.0.0"] {
        let out = checker().args(["--host", host]).output().unwrap();
        let (_, stderr) = text(&out);
        assert_eq!(out.status.code(), Some(2), "{host}: {stderr}");
        assert!(stderr.contains("loopback"), "{stderr}");
    }
    let out = checker().output().unwrap();
    assert_eq!(out.status.code(), Some(2), "no host");
}
