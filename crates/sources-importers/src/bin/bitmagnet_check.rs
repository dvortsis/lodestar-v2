//! Checks a restored bitmagnet database before anything imports from it
//! (`docs/operations/imports/bitmagnet-restore.md`).
//!
//! It connects read-only, over a Unix socket or loopback only, and checks:
//! the server is PostgreSQL 16; the database is `SQL_ASCII`; the tables and
//! columns `bitmagnet.rs` reads exist with bitmagnet 0.10.0's types; row
//! counts; a sample of `name` and `path` values, hashed as the raw bytes
//! PostgreSQL stores (never decoded); and free disk on the data volume.
//!
//! The database's content is untrusted: text is only ever fetched as
//! `bytea`, every name it supplies is escaped before it's printed, and every
//! query is bounded by a row limit and a statement timeout.
//!
//! JSON goes to stdout and a human summary to stderr. Exit codes: 0 every
//! check passed, 1 a check failed, 2 bad usage or no connection.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use tokio_postgres::config::SslMode;
use tokio_postgres::{Client, Config, NoTls, Row, types::ToSql};

const USAGE: &str = "\
Usage: bitmagnet_check --host <socket dir | 127.0.0.1 | ::1 | localhost> [options]

Connection (each falls back to the libpq variable named):
  --host <h>               Unix socket directory or a loopback address (PGHOST)
  --port <n>               (PGPORT, default 5432)
  --user <name>            (PGUSER, default bitmagnet_reader)
  --dbname <name>          (PGDATABASE, default bitmagnet)
  --password-file <path>   read the password from this file; otherwise
                           PGPASSWORD is used. There is no --password.
Checks:
  --sample <n>             values sampled per table (default 1000, max 100000)
  --seed <n>               sample seed; the same seed samples the same rows
                           (default 0)
  --encoding <name>        the encoding the database must declare (default
                           SQL_ASCII, as the dump does)
  --counts exact|estimate  exact count(*) of the imported tables, or the
                           planner's estimate only (default exact)
  --data-dir <path>        the volume holding the restored cluster
  --reserve <size>         free space it must keep, e.g. 50GiB (needs --data-dir)
  --statement-timeout <s>  per-query limit in seconds (default 600)

JSON goes to stdout, a summary to stderr.
Exit: 0 all checks passed, 1 a check failed, 2 bad usage or no connection.
";

/// What `bitmagnet.rs` reads, with the types bitmagnet 0.10.0 declares
/// (`udt_name`). A missing column or a different type fails the check.
const REQUIRED: &[(&str, &[(&str, &str)])] = &[
    (
        "torrents",
        &[
            ("info_hash", "bytea"),
            ("name", "text"),
            ("size", "int8"),
            ("private", "bool"),
            ("files_status", "FilesStatus"),
            ("files_count", "int4"),
            ("created_at", "timestamptz"),
        ],
    ),
    (
        "torrent_files",
        &[
            ("info_hash", "bytea"),
            ("index", "int4"),
            ("path", "text"),
            ("size", "int8"),
        ],
    ),
    (
        "torrent_pieces",
        &[
            ("info_hash", "bytea"),
            ("piece_length", "int8"),
            ("pieces", "bytea"),
        ],
    ),
];

/// The rest of those tables' 0.10.0 columns: present, but not read.
const KNOWN_UNREAD: &[(&str, &[&str])] = &[
    ("torrents", &["updated_at", "extension"]),
    ("torrent_files", &["extension", "created_at", "updated_at"]),
    ("torrent_pieces", &["created_at"]),
];

/// Every other table bitmagnet 0.10.0's migrations create. Anything else in
/// the schema is listed as an extra, which isn't a failure.
const KNOWN_TABLES: &[&str] = &[
    "bloom_filters",
    "content",
    "content_attributes",
    "content_collections",
    "content_collections_content",
    "goose_db_version",
    "key_values",
    "metadata_sources",
    "queue_jobs",
    "torrent_contents",
    "torrent_hints",
    "torrent_sources",
    "torrent_tags",
    "torrents_torrent_sources",
];

const FILES_STATUS: &[&str] = &["no_info", "single", "multi", "over_threshold"];

/// Sampled text columns: table, its unique ordering key, the text column.
const SAMPLED: &[(&str, &str, &str)] = &[
    ("torrents", "info_hash", "name"),
    ("torrent_files", "info_hash, \"index\"", "path"),
];

/// Starting points per sample: the first is the lowest key, the rest are
/// spread from the seed, so a sample covers the whole key range.
const SEGMENTS: usize = 8;

const MAX_SAMPLE: u32 = 100_000;
/// Bounds on the catalog queries, which a hostile database could inflate.
const MAX_CATALOG_ROWS: i64 = 10_000;
const MAX_LISTED: usize = 200;

#[derive(Debug)]
struct Args {
    host: String,
    port: u16,
    user: String,
    dbname: String,
    password_file: Option<PathBuf>,
    sample: u32,
    seed: u64,
    encoding: String,
    exact_counts: bool,
    data_dir: Option<PathBuf>,
    reserve: Option<u64>,
    timeout: Duration,
}

/// Parses argv (without the program name) with the environment as fallback.
/// Errors never quote a value from argv, in case it's a secret.
fn parse_args(
    argv: impl IntoIterator<Item = String>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<Args>, String> {
    let mut a = Args {
        host: env("PGHOST").unwrap_or_default(),
        port: 5432,
        user: env("PGUSER").unwrap_or_else(|| "bitmagnet_reader".into()),
        dbname: env("PGDATABASE").unwrap_or_else(|| "bitmagnet".into()),
        password_file: None,
        sample: 1000,
        seed: 0,
        encoding: "SQL_ASCII".into(),
        exact_counts: true,
        data_dir: None,
        reserve: None,
        timeout: Duration::from_secs(600),
    };
    if let Some(p) = env("PGPORT") {
        a.port = p.parse().map_err(|_| "PGPORT is not a port number")?;
    }
    let mut it = argv.into_iter();
    while let Some(flag) = it.next() {
        if flag == "-h" || flag == "--help" {
            return Ok(None);
        }
        if flag == "--password" || flag.starts_with("--password=") {
            return Err(
                "there is no --password: argv is visible to other processes. \
                 Use --password-file or PGPASSWORD"
                    .into(),
            );
        }
        if !flag.starts_with("--") || flag.contains('=') {
            return Err("unexpected argument (flags are --name value)".into());
        }
        let known = [
            "--host",
            "--port",
            "--user",
            "--dbname",
            "--password-file",
            "--sample",
            "--seed",
            "--encoding",
            "--counts",
            "--data-dir",
            "--reserve",
            "--statement-timeout",
        ];
        let Some(&flag) = known.iter().find(|k| **k == flag) else {
            return Err("unknown flag (see --help)".into());
        };
        let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let bad = || format!("{flag}: invalid value");
        match flag {
            "--host" => a.host = v,
            "--port" => a.port = v.parse().map_err(|_| bad())?,
            "--user" => a.user = v,
            "--dbname" => a.dbname = v,
            "--password-file" => a.password_file = Some(v.into()),
            "--sample" => {
                a.sample = v.parse().map_err(|_| bad())?;
                if a.sample == 0 || a.sample > MAX_SAMPLE {
                    return Err(format!("--sample: 1 to {MAX_SAMPLE}"));
                }
            }
            "--seed" => a.seed = v.parse().map_err(|_| bad())?,
            "--encoding" => a.encoding = v,
            "--counts" => {
                a.exact_counts = match v.as_str() {
                    "exact" => true,
                    "estimate" => false,
                    _ => return Err(bad()),
                }
            }
            "--data-dir" => a.data_dir = Some(v.into()),
            "--reserve" => a.reserve = Some(parse_size(&v).ok_or_else(bad)?),
            "--statement-timeout" => {
                let s: u64 = v.parse().map_err(|_| bad())?;
                if s == 0 || s > 86_400 {
                    return Err(bad());
                }
                a.timeout = Duration::from_secs(s);
            }
            _ => unreachable!("listed in known"),
        }
    }
    if a.host.is_empty() {
        return Err("--host (or PGHOST) is required".into());
    }
    if a.reserve.is_some() && a.data_dir.is_none() {
        return Err("--reserve needs --data-dir".into());
    }
    Ok(Some(a))
}

/// `123`, `10GiB`, `512MiB`, `1TiB`, `64KiB` (binary units; `B` optional).
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: u64 = n.parse().ok()?;
    let shift = match unit.trim() {
        "" | "B" => 0,
        "KiB" => 10,
        "MiB" => 20,
        "GiB" => 30,
        "TiB" => 40,
        _ => return None,
    };
    n.checked_mul(1u64 << shift)
}

/// Where to connect. Plain (non-TLS) connections are allowed only to a Unix
/// socket or a loopback address; this checker has no TLS.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    Socket(PathBuf),
    Loopback(IpAddr),
}

fn target(host: &str, port: u16) -> Result<Target, String> {
    if host.starts_with('/') {
        return Ok(Target::Socket(host.into()));
    }
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return if ip.is_loopback() {
            Ok(Target::Loopback(ip))
        } else {
            Err(not_loopback())
        };
    }
    if host == "localhost" {
        // Every address it resolves to must be loopback; connect to the
        // first, so the name can't resolve differently a moment later.
        let addrs: Vec<IpAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|_| "localhost doesn't resolve".to_string())?
            .map(|a| a.ip())
            .collect();
        return match addrs.first() {
            Some(ip) if addrs.iter().all(IpAddr::is_loopback) => Ok(Target::Loopback(*ip)),
            _ => Err(not_loopback()),
        };
    }
    Err(not_loopback())
}

fn not_loopback() -> String {
    "the host must be a Unix socket directory or a loopback address: this \
     checker has no TLS, and the restored database stays off the network"
        .into()
}

fn read_password(
    args: &Args,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<Vec<u8>>, String> {
    let Some(path) = &args.password_file else {
        return Ok(env("PGPASSWORD").map(String::into_bytes));
    };
    let mut bytes = std::fs::read(path).map_err(|e| format!("--password-file: {e}"))?;
    // One trailing newline, as `echo` or an editor leaves it.
    if bytes.ends_with(b"\n") {
        bytes.pop();
        if bytes.ends_with(b"\r") {
            bytes.pop();
        }
    }
    if bytes.is_empty() {
        return Err("--password-file is empty".into());
    }
    Ok(Some(bytes))
}

/// The password file's permissions, when they let others read it.
#[cfg(unix)]
fn password_file_warning(args: &Args) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(args.password_file.as_ref()?)
        .ok()?
        .permissions()
        .mode();
    (mode & 0o077 != 0).then(|| {
        format!(
            "the password file is readable by others (mode {:o}); chmod 600 it",
            mode & 0o777
        )
    })
}

#[cfg(not(unix))]
fn password_file_warning(_: &Args) -> Option<String> {
    None
}

async fn connect(args: &Args, password: Option<&[u8]>) -> Result<Client, String> {
    let mut cfg = Config::new();
    cfg.user(&args.user)
        .dbname(&args.dbname)
        .port(args.port)
        .application_name("lodestar-bitmagnet-check")
        .connect_timeout(Duration::from_secs(10))
        .ssl_mode(SslMode::Disable)
        // Set before the first statement runs. Only our own numbers go in.
        .options(format!(
            "-c statement_timeout={} -c lock_timeout=10000 \
             -c idle_in_transaction_session_timeout={} -c default_transaction_read_only=on",
            args.timeout.as_millis(),
            args.timeout.as_millis() + 60_000,
        ));
    if let Some(p) = password {
        cfg.password(p);
    }
    match target(&args.host, args.port)? {
        #[cfg(unix)]
        Target::Socket(dir) => {
            cfg.host_path(dir);
        }
        #[cfg(not(unix))]
        Target::Socket(_) => return Err("Unix sockets need Linux or macOS".into()),
        Target::Loopback(ip) => {
            cfg.host(ip.to_string()).hostaddr(ip);
        }
    }
    let (client, connection) = tokio::time::timeout(Duration::from_secs(20), cfg.connect(NoTls))
        .await
        .map_err(|_| "connecting timed out".to_string())?
        .map_err(|e| format!("couldn't connect: {}", pg_error(&e)))?;
    tokio::spawn(connection);
    // Belt and braces: the session refuses writes even if the role could.
    client
        .batch_execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
        .await
        .map_err(|e| format!("couldn't set up the session: {}", pg_error(&e)))?;
    Ok(client)
}

/// The server's message where there is one: tokio-postgres alone says only
/// "db error". It never contains the password.
fn pg_error(e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => format!("{} ({})", db.message(), db.code().code()),
        None => e.to_string(),
    }
}

/// One query, also bounded on our side in case the server stops answering.
async fn query(
    client: &Client,
    timeout: Duration,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
) -> Result<Vec<Row>, String> {
    match tokio::time::timeout(timeout + Duration::from_secs(30), client.query(sql, params)).await {
        Ok(Ok(rows)) => Ok(rows),
        Ok(Err(e)) => Err(pg_error(&e)),
        Err(_) => Err("the query timed out".into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Fail,
    Skipped,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Fail => "fail",
            Status::Skipped => "skipped",
        }
    }
}

#[derive(Debug)]
struct Check {
    name: &'static str,
    status: Status,
    detail: String,
}

#[derive(Debug, Default)]
struct Schema {
    missing: Vec<String>,
    /// `(table.column, expected, found)`.
    wrong_type: Vec<(String, String, String)>,
    extra_tables: Vec<String>,
    extra_columns: Vec<String>,
    extra_enum_labels: Vec<String>,
    missing_enum_labels: Vec<String>,
}

#[derive(Debug)]
struct TableCount {
    name: String,
    estimate: Option<i64>,
    exact: Option<i64>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Sample {
    table: &'static str,
    column: &'static str,
    rows: u64,
    nulls: u64,
    utf8_valid: u64,
    non_ascii: u64,
    bytes: u64,
    sha256: String,
}

#[derive(Debug)]
struct Disk {
    path: String,
    available: u64,
    reserve: Option<u64>,
}

#[derive(Debug, Default)]
struct Report {
    checks: Vec<Check>,
    warnings: Vec<String>,
    server_version_num: Option<i32>,
    server_version: Option<String>,
    encoding: Option<String>,
    collate: Option<String>,
    ctype: Option<String>,
    database_bytes: Option<i64>,
    schema: Option<Schema>,
    tables: Vec<TableCount>,
    samples: Vec<Sample>,
    disk: Option<Disk>,
}

impl Report {
    fn check(&mut self, name: &'static str, status: Status, detail: impl Into<String>) {
        self.checks.push(Check {
            name,
            status,
            detail: detail.into(),
        });
    }

    fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.status != Status::Fail)
    }
}

/// Bytes from the database, for display: lossy, with control and bidi
/// characters escaped so a hostile name can't drive or disguise the terminal.
fn shown(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in String::from_utf8_lossy(raw).chars() {
        let bidi = matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
        if c.is_control() || bidi {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

async fn check_server(client: &Client, args: &Args, r: &mut Report) -> Result<(), String> {
    let rows = query(
        client,
        args.timeout,
        "SELECT current_setting('server_version_num')::int,
                convert_to(current_setting('server_version'), 'SQL_ASCII')",
        &[],
    )
    .await?;
    let num: i32 = rows[0].get(0);
    r.server_version_num = Some(num);
    r.server_version = Some(shown(rows[0].get(1)));
    if (160_000..170_000).contains(&num) {
        r.check(
            "server_version",
            Status::Ok,
            format!("PostgreSQL {}", r.server_version.as_deref().unwrap_or("?")),
        );
    } else {
        r.check(
            "server_version",
            Status::Fail,
            format!("PostgreSQL 16 expected, server is {num}"),
        );
    }
    Ok(())
}

async fn check_encoding(client: &Client, args: &Args, r: &mut Report) -> Result<(), String> {
    let rows = query(
        client,
        args.timeout,
        "SELECT convert_to(pg_encoding_to_char(encoding)::text, 'SQL_ASCII'),
                convert_to(datcollate::text, 'SQL_ASCII'),
                convert_to(datctype::text, 'SQL_ASCII'),
                pg_database_size(oid)
         FROM pg_database WHERE datname = current_database()",
        &[],
    )
    .await?;
    let Some(row) = rows.first() else {
        r.check(
            "encoding",
            Status::Fail,
            "the database isn't in pg_database",
        );
        return Ok(());
    };
    let encoding = shown(row.get(0));
    r.collate = Some(shown(row.get(1)));
    r.ctype = Some(shown(row.get(2)));
    r.database_bytes = Some(row.get(3));
    if encoding == args.encoding {
        r.check("encoding", Status::Ok, encoding.clone());
    } else {
        r.check(
            "encoding",
            Status::Fail,
            format!(
                "{} expected, the database is {encoding}: the restore transcoded, or this is the wrong database",
                shown(args.encoding.as_bytes())
            ),
        );
    }
    r.encoding = Some(encoding);
    Ok(())
}

async fn check_schema(client: &Client, args: &Args, r: &mut Report) -> Result<(), String> {
    let rows = query(
        client,
        args.timeout,
        "SELECT convert_to(c.relname::text, 'SQL_ASCII'), convert_to(a.attname::text, 'SQL_ASCII'),
                convert_to(t.typname::text, 'SQL_ASCII')
         FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace
         JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
         JOIN pg_type t ON t.oid = a.atttypid
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p')
         ORDER BY 1, a.attnum LIMIT $1",
        &[&MAX_CATALOG_ROWS],
    )
    .await?;
    if rows.len() as i64 >= MAX_CATALOG_ROWS {
        r.warnings.push(format!(
            "over {MAX_CATALOG_ROWS} columns in schema public; only the first were read"
        ));
    }
    let cols: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = rows
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect();
    let mut s = Schema::default();
    for (table, columns) in REQUIRED {
        for (column, ty) in *columns {
            match cols
                .iter()
                .find(|(t, c, _)| t == table.as_bytes() && c == column.as_bytes())
            {
                None => s.missing.push(format!("{table}.{column}")),
                Some((_, _, found)) if found != ty.as_bytes() => {
                    s.wrong_type
                        .push((format!("{table}.{column}"), ty.to_string(), shown(found)))
                }
                Some(_) => {}
            }
        }
    }
    let tables: BTreeSet<&[u8]> = cols.iter().map(|(t, _, _)| t.as_slice()).collect();
    for t in &tables {
        let known = REQUIRED.iter().any(|(k, _)| k.as_bytes() == *t)
            || KNOWN_TABLES.iter().any(|k| k.as_bytes() == *t);
        if !known && s.extra_tables.len() < MAX_LISTED {
            s.extra_tables.push(shown(t));
        }
    }
    for (t, c, _) in &cols {
        let Some((table, required)) = REQUIRED.iter().find(|(k, _)| k.as_bytes() == t) else {
            continue;
        };
        let unread = KNOWN_UNREAD
            .iter()
            .find(|(k, _)| k == table)
            .map_or(&[][..], |(_, cs)| cs);
        let known = required.iter().any(|(k, _)| k.as_bytes() == c)
            || unread.iter().any(|k| k.as_bytes() == c);
        if !known && s.extra_columns.len() < MAX_LISTED {
            s.extra_columns.push(format!("{table}.{}", shown(c)));
        }
    }
    let labels: Vec<Vec<u8>> = query(
        client,
        args.timeout,
        "SELECT convert_to(e.enumlabel::text, 'SQL_ASCII')
         FROM pg_enum e JOIN pg_type t ON t.oid = e.enumtypid
         JOIN pg_namespace n ON n.oid = t.typnamespace
         WHERE t.typname = 'FilesStatus' AND n.nspname = 'public'
         ORDER BY e.enumsortorder LIMIT $1",
        &[&(MAX_LISTED as i64)],
    )
    .await?
    .iter()
    .map(|row| row.get(0))
    .collect();
    for want in FILES_STATUS {
        if !labels.iter().any(|l| l == want.as_bytes()) {
            s.missing_enum_labels.push(want.to_string());
        }
    }
    for l in &labels {
        if !FILES_STATUS.iter().any(|w| w.as_bytes() == l) {
            s.extra_enum_labels.push(shown(l));
        }
    }
    let ok = s.missing.is_empty() && s.wrong_type.is_empty() && s.missing_enum_labels.is_empty();
    let extras = s.extra_tables.len() + s.extra_columns.len() + s.extra_enum_labels.len();
    if ok {
        r.check(
            "schema",
            Status::Ok,
            format!("bitmagnet 0.10.0 tables and columns present; {extras} extras listed"),
        );
    } else {
        let mut why: Vec<String> = s.missing.iter().map(|m| format!("missing {m}")).collect();
        why.extend(
            s.wrong_type
                .iter()
                .map(|(c, want, found)| format!("{c} is {found}, expected {want}")),
        );
        why.extend(
            s.missing_enum_labels
                .iter()
                .map(|l| format!("FilesStatus lacks '{l}'")),
        );
        r.check("schema", Status::Fail, why.join("; "));
    }
    r.schema = Some(s);
    Ok(())
}

async fn check_counts(client: &Client, args: &Args, r: &mut Report) -> Result<(), String> {
    let rows = query(
        client,
        args.timeout,
        "SELECT convert_to(c.relname::text, 'SQL_ASCII'), c.reltuples::bigint
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p')
         ORDER BY 1 LIMIT $1",
        &[&(MAX_LISTED as i64)],
    )
    .await?;
    for row in &rows {
        let estimate: i64 = row.get(1);
        r.tables.push(TableCount {
            name: shown(row.get(0)),
            // -1: never vacuumed or analyzed, so no estimate.
            estimate: (estimate >= 0).then_some(estimate),
            exact: None,
        });
    }
    if !args.exact_counts {
        r.check(
            "row_counts",
            Status::Skipped,
            "estimates only (--counts estimate)",
        );
        return Ok(());
    }
    let mut failed = Vec::new();
    let mut torrents = None;
    for (table, _) in REQUIRED {
        let Some(t) = r.tables.iter_mut().find(|t| t.name == *table) else {
            failed.push(format!("no {table} table"));
            continue;
        };
        // The table name is one of ours, never the database's.
        match query(
            client,
            args.timeout,
            &format!("SELECT count(*) FROM public.{table}"),
            &[],
        )
        .await
        {
            Ok(rows) => {
                let n: i64 = rows[0].get(0);
                t.exact = Some(n);
                if *table == "torrents" {
                    torrents = Some(n);
                }
            }
            Err(e) => failed.push(format!(
                "{table}: {e} (raise --statement-timeout, or use --counts estimate)"
            )),
        }
    }
    if torrents == Some(0) {
        failed.push("torrents is empty".into());
    }
    if failed.is_empty() {
        r.check(
            "row_counts",
            Status::Ok,
            format!("{} torrents", torrents.unwrap_or(0)),
        );
    } else {
        r.check("row_counts", Status::Fail, failed.join("; "));
    }
    Ok(())
}

/// splitmix64: spreads a seed into well-mixed starting keys.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The sample's starting info_hashes: the empty key (so the lowest rows are
/// always in), then `SEGMENTS - 1` keys from the seed. info_hashes are
/// uniform, so rows following a random key are a random sample, read from
/// the primary key index rather than by scanning the table.
fn starts(seed: u64) -> Vec<Vec<u8>> {
    let mut state = seed;
    let mut out = vec![Vec::new()];
    for _ in 1..SEGMENTS {
        let mut key = Vec::with_capacity(24);
        for _ in 0..3 {
            key.extend_from_slice(&splitmix(&mut state).to_be_bytes());
        }
        key.truncate(20);
        out.push(key);
    }
    out
}

/// The SQL for one sample. Rows come back ordered by the table's key; the
/// last column of every row repeats the aggregates. An empty sample still
/// returns one row, with a NULL key.
///
/// `sha256` covers the sampled values in key order: for each, `0x00` if it
/// is NULL, else `0x01` followed by SHA-256 of its stored bytes. The bytes
/// come from `convert_to(…, 'SQL_ASCII')`, which converts nothing, so it's
/// what is on disk whatever the database's encoding.
fn sample_sql(table: &str, key: &str, column: &str) -> String {
    let first_key = key.split(',').next().unwrap_or(key).trim();
    let order = key
        .split(',')
        .map(|k| format!("k.{}", k.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "WITH starts(s) AS (SELECT unnest($1::bytea[])),
         picked AS (
           SELECT DISTINCT ON ({order}) k.*
           FROM starts CROSS JOIN LATERAL (
             SELECT {key}, convert_to(t.{column}, 'SQL_ASCII') AS v
             FROM public.{table} t WHERE t.{first_key} >= starts.s
             ORDER BY {key} LIMIT $2) k
           ORDER BY {order} LIMIT $3),
         agg AS (
           SELECT count(*) AS n, coalesce(sum(length(v)), 0)::bigint AS bytes,
             encode(sha256(coalesce(string_agg(
               CASE WHEN v IS NULL THEN '\\x00'::bytea ELSE '\\x01'::bytea || sha256(v) END,
               ''::bytea ORDER BY {order_plain}), ''::bytea)), 'hex') AS digest
           FROM picked k)
         SELECT p.{first_key}, p.v, agg.n, agg.bytes, agg.digest
         FROM agg LEFT JOIN picked p ON true
         ORDER BY {order_p}",
        order_plain = order,
        order_p = key
            .split(',')
            .map(|k| format!("p.{}", k.trim()))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

async fn check_samples(client: &Client, args: &Args, r: &mut Report) -> Result<(), String> {
    let schema_ok = r
        .checks
        .iter()
        .any(|c| c.name == "schema" && c.status == Status::Ok);
    if !schema_ok {
        r.check("samples", Status::Skipped, "the schema check failed");
        return Ok(());
    }
    let starts = starts(args.seed);
    let per_segment = i64::from(args.sample.div_ceil(SEGMENTS as u32));
    let total = i64::from(args.sample);
    let mut failed = Vec::new();
    for (table, key, column) in SAMPLED {
        let sql = sample_sql(table, key, column);
        let rows = match query(client, args.timeout, &sql, &[&starts, &per_segment, &total]).await {
            Ok(rows) => rows,
            Err(e) => {
                failed.push(format!("{table}.{column}: {e}"));
                continue;
            }
        };
        let mut s = Sample {
            table,
            column,
            ..Sample::default()
        };
        for row in &rows {
            if row.get::<_, Option<&[u8]>>(0).is_none() {
                continue; // the empty sample's placeholder row
            }
            s.rows += 1;
            // Validated in place: the bytes are never turned into a String.
            match row.get::<_, Option<&[u8]>>(1) {
                None => s.nulls += 1,
                Some(v) => {
                    s.bytes += v.len() as u64;
                    s.utf8_valid += u64::from(std::str::from_utf8(v).is_ok());
                    s.non_ascii += u64::from(!v.is_ascii());
                }
            }
        }
        let (n, bytes, digest): (i64, i64, String) =
            (rows[0].get(2), rows[0].get(3), rows[0].get(4));
        if n as u64 != s.rows || bytes as u64 != s.bytes {
            failed.push(format!(
                "{table}.{column}: the server's totals don't match the rows read"
            ));
        }
        s.sha256 = digest;
        r.samples.push(s);
    }
    if failed.is_empty() {
        let detail = r
            .samples
            .iter()
            .map(|s| {
                format!(
                    "{}.{}: {} sampled, {} valid UTF-8",
                    s.table, s.column, s.rows, s.utf8_valid
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        r.check("samples", Status::Ok, detail);
    } else {
        r.check("samples", Status::Fail, failed.join("; "));
    }
    Ok(())
}

/// The "Available" column of `df -P -k` output, in bytes.
fn parse_df(out: &str) -> Option<u64> {
    let line = out.lines().filter(|l| !l.trim().is_empty()).nth(1)?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    // Filesystem, blocks, used, available, capacity%, mount point. The
    // first and last can hold spaces, so find the capacity column instead.
    let cap = fields
        .iter()
        .position(|f| f.ends_with('%') && f[..f.len() - 1].bytes().all(|b| b.is_ascii_digit()))?;
    let kib: u64 = fields.get(cap.checked_sub(1)?)?.parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(unix)]
fn available_bytes(path: &std::path::Path) -> Result<u64, String> {
    // POSIX df, so no new dependency: the same on Unraid, in a container and
    // in Docker Desktop's Linux VM. No shell; `--` ends df's options.
    let out = std::process::Command::new("df")
        .args(["-P", "-k", "--"])
        .arg(path)
        .output()
        .map_err(|e| format!("couldn't run df: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "df failed: {}",
            shown(String::from_utf8_lossy(&out.stderr).trim().as_bytes())
        ));
    }
    parse_df(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "couldn't read df's output".into())
}

#[cfg(not(unix))]
fn available_bytes(_: &std::path::Path) -> Result<u64, String> {
    Err("run the checker in its Linux container to measure the volume (see the runbook)".into())
}

fn check_disk(args: &Args, r: &mut Report) {
    let Some(path) = &args.data_dir else {
        r.check("disk", Status::Skipped, "no --data-dir given");
        return;
    };
    match available_bytes(path) {
        Ok(available) => {
            let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
            match args.reserve {
                Some(reserve) if available < reserve => r.check(
                    "disk",
                    Status::Fail,
                    format!(
                        "{:.1} GiB free, below the {:.1} GiB reserve",
                        gib(available),
                        gib(reserve)
                    ),
                ),
                Some(reserve) => r.check(
                    "disk",
                    Status::Ok,
                    format!(
                        "{:.1} GiB free, reserve {:.1} GiB",
                        gib(available),
                        gib(reserve)
                    ),
                ),
                None => r.check(
                    "disk",
                    Status::Ok,
                    format!("{:.1} GiB free, no reserve set", gib(available)),
                ),
            }
            r.disk = Some(Disk {
                path: shown(path.as_os_str().as_encoded_bytes()),
                available,
                reserve: args.reserve,
            });
        }
        Err(e) => r.check("disk", Status::Fail, e),
    }
}

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_opt_str(out: &mut String, s: Option<&str>) {
    match s {
        Some(s) => json_str(out, s),
        None => out.push_str("null"),
    }
}

fn json_opt_num(out: &mut String, n: Option<impl std::fmt::Display>) {
    match n {
        Some(n) => {
            let _ = write!(out, "{n}");
        }
        None => out.push_str("null"),
    }
}

fn json_list(out: &mut String, items: &[String]) {
    out.push('[');
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_str(out, s);
    }
    out.push(']');
}

/// The report as JSON. Field names and order are stable (`"format": 1`).
fn to_json(r: &Report) -> String {
    let mut o = String::new();
    let _ = write!(o, "{{\"format\":1,\"ok\":{},\"checks\":[", r.ok());
    for (i, c) in r.checks.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"name\":");
        json_str(&mut o, c.name);
        o.push_str(",\"status\":");
        json_str(&mut o, c.status.as_str());
        o.push_str(",\"detail\":");
        json_str(&mut o, &c.detail);
        o.push('}');
    }
    o.push_str("],\"warnings\":");
    json_list(&mut o, &r.warnings);
    o.push_str(",\"server\":{\"version_num\":");
    json_opt_num(&mut o, r.server_version_num);
    o.push_str(",\"version\":");
    json_opt_str(&mut o, r.server_version.as_deref());
    o.push_str("},\"database\":{\"encoding\":");
    json_opt_str(&mut o, r.encoding.as_deref());
    o.push_str(",\"collate\":");
    json_opt_str(&mut o, r.collate.as_deref());
    o.push_str(",\"ctype\":");
    json_opt_str(&mut o, r.ctype.as_deref());
    o.push_str(",\"size_bytes\":");
    json_opt_num(&mut o, r.database_bytes);
    o.push_str("},\"schema\":");
    match &r.schema {
        None => o.push_str("null"),
        Some(s) => {
            o.push_str("{\"missing\":");
            json_list(&mut o, &s.missing);
            o.push_str(",\"wrong_type\":[");
            for (i, (c, want, found)) in s.wrong_type.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push_str("{\"column\":");
                json_str(&mut o, c);
                o.push_str(",\"expected\":");
                json_str(&mut o, want);
                o.push_str(",\"found\":");
                json_str(&mut o, found);
                o.push('}');
            }
            o.push_str("],\"missing_enum_labels\":");
            json_list(&mut o, &s.missing_enum_labels);
            o.push_str(",\"extra_tables\":");
            json_list(&mut o, &s.extra_tables);
            o.push_str(",\"extra_columns\":");
            json_list(&mut o, &s.extra_columns);
            o.push_str(",\"extra_enum_labels\":");
            json_list(&mut o, &s.extra_enum_labels);
            o.push('}');
        }
    }
    o.push_str(",\"tables\":[");
    for (i, t) in r.tables.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"name\":");
        json_str(&mut o, &t.name);
        o.push_str(",\"rows_estimate\":");
        json_opt_num(&mut o, t.estimate);
        o.push_str(",\"rows_exact\":");
        json_opt_num(&mut o, t.exact);
        o.push('}');
    }
    o.push_str("],\"samples\":[");
    for (i, s) in r.samples.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        let _ = write!(
            o,
            "{{\"table\":\"{}\",\"column\":\"{}\",\"rows\":{},\"nulls\":{},\"utf8_valid\":{},\
             \"non_ascii\":{},\"bytes\":{},\"sha256\":",
            s.table, s.column, s.rows, s.nulls, s.utf8_valid, s.non_ascii, s.bytes
        );
        json_str(&mut o, &s.sha256);
        o.push('}');
    }
    o.push_str("],\"disk\":");
    match &r.disk {
        None => o.push_str("null"),
        Some(d) => {
            o.push_str("{\"path\":");
            json_str(&mut o, &d.path);
            let _ = write!(o, ",\"available_bytes\":{},\"reserve_bytes\":", d.available);
            json_opt_num(&mut o, d.reserve);
            o.push('}');
        }
    }
    o.push('}');
    o
}

fn summary(r: &Report, seed: u64) -> String {
    let mut o = String::new();
    for c in &r.checks {
        let mark = match c.status {
            Status::Ok => "ok  ",
            Status::Fail => "FAIL",
            Status::Skipped => "skip",
        };
        let _ = writeln!(o, "{mark} {:<15} {}", c.name, c.detail);
    }
    for w in &r.warnings {
        let _ = writeln!(o, "warn {w}");
    }
    if let Some(s) = &r.schema {
        for (what, list) in [
            ("extra table", &s.extra_tables),
            ("extra column", &s.extra_columns),
            ("extra FilesStatus", &s.extra_enum_labels),
        ] {
            for item in list {
                let _ = writeln!(o, "     {what}: {item}");
            }
        }
    }
    for t in &r.tables {
        let n = |v: Option<i64>| v.map_or("-".to_string(), |v| v.to_string());
        let _ = writeln!(
            o,
            "     rows {:<28} exact {:>12}  estimate {:>12}",
            t.name,
            n(t.exact),
            n(t.estimate)
        );
    }
    for s in &r.samples {
        let _ = writeln!(
            o,
            "     sample {}.{} (seed {seed}): {} values, {} NULL, {} valid UTF-8, {} non-ASCII, sha256 {}",
            s.table, s.column, s.rows, s.nulls, s.utf8_valid, s.non_ascii, s.sha256
        );
    }
    let _ = writeln!(
        o,
        "{}",
        if r.ok() {
            "all checks passed"
        } else {
            "a check FAILED"
        }
    );
    o
}

async fn run(args: &Args, password: Option<&[u8]>) -> Result<Report, String> {
    let client = connect(args, password).await?;
    let mut r = Report::default();
    if let Some(w) = password_file_warning(args) {
        r.warnings.push(w);
    }
    check_server(&client, args, &mut r).await?;
    check_encoding(&client, args, &mut r).await?;
    check_schema(&client, args, &mut r).await?;
    check_counts(&client, args, &mut r).await?;
    check_samples(&client, args, &mut r).await?;
    check_disk(args, &mut r);
    Ok(r)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let env = |k: &str| std::env::var(k).ok();
    let args = match parse_args(std::env::args().skip(1), env) {
        Ok(Some(a)) => a,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("bitmagnet_check: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let password = match read_password(&args, env) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bitmagnet_check: {e}");
            return ExitCode::from(2);
        }
    };
    match run(&args, password.as_deref()).await {
        Ok(r) => {
            println!("{}", to_json(&r));
            eprint!("{}", summary(&r, args.seed));
            ExitCode::from(if r.ok() { 0 } else { 1 })
        }
        Err(e) => {
            eprintln!("bitmagnet_check: {}", shown(e.as_bytes()));
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Result<Option<Args>, String> {
        parse_args(v.iter().map(|s| s.to_string()), |_| None)
    }

    #[test]
    fn parses_flags_and_env() {
        let a = args(&[
            "--host",
            "/run/pg",
            "--sample",
            "50",
            "--reserve",
            "2GiB",
            "--data-dir",
            "/d",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            (a.host.as_str(), a.sample, a.reserve),
            ("/run/pg", 50, Some(2 << 30))
        );
        assert_eq!(
            (a.user.as_str(), a.dbname.as_str(), a.port),
            ("bitmagnet_reader", "bitmagnet", 5432)
        );
        let env = |k: &str| match k {
            "PGHOST" => Some("127.0.0.1".into()),
            "PGPORT" => Some("5499".into()),
            _ => None,
        };
        let a = parse_args(Vec::<String>::new(), env).unwrap().unwrap();
        assert_eq!((a.host.as_str(), a.port), ("127.0.0.1", 5499));
        assert!(args(&["--help"]).unwrap().is_none());
    }

    #[test]
    fn usage_errors_never_quote_argv() {
        for argv in [
            &["--host", "/s", "--password", "hunter2"][..],
            &["--host", "/s", "--password=hunter2"],
            &["--host", "/s", "hunter2"],
            &["--host", "/s", "--hunter2", "x"],
            &["--host", "/s", "--port", "hunter2"],
            &["--host", "/s", "--host=hunter2"],
        ] {
            let e = args(argv).unwrap_err();
            assert!(!e.contains("hunter2"), "{e}");
        }
        assert!(args(&[]).unwrap_err().contains("--host"));
        assert!(args(&["--host", "/s", "--sample", "0"]).is_err());
        assert!(
            args(&["--host", "/s", "--reserve", "1GiB"]).is_err(),
            "reserve needs a dir"
        );
        assert!(args(&["--host", "/s", "--counts", "most"]).is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("123"), Some(123));
        assert_eq!(parse_size("64KiB"), Some(64 << 10));
        assert_eq!(parse_size("50GiB"), Some(50 << 30));
        assert_eq!(parse_size("1TiB"), Some(1 << 40));
        assert_eq!(parse_size("5GB"), None, "binary units only");
        assert_eq!(parse_size("GiB"), None);
        assert_eq!(parse_size("99999999999TiB"), None, "overflow");
    }

    #[test]
    fn only_sockets_and_loopback() {
        assert_eq!(
            target("/run/pg", 5432),
            Ok(Target::Socket("/run/pg".into()))
        );
        assert_eq!(
            target("127.0.0.1", 5432),
            Ok(Target::Loopback([127, 0, 0, 1].into()))
        );
        assert!(matches!(target("[::1]", 5432), Ok(Target::Loopback(ip)) if ip.is_loopback()));
        assert!(matches!(target("localhost", 5432), Ok(Target::Loopback(ip)) if ip.is_loopback()));
        for host in ["10.0.0.5", "0.0.0.0", "db.example", "postgres", "::"] {
            assert!(target(host, 5432).is_err(), "{host}");
        }
    }

    #[test]
    fn df_output() {
        let out = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n\
                   /dev/sda1        1000000    400000    600000      40% /mnt/user\n";
        assert_eq!(parse_df(out), Some(600_000 * 1024));
        let spaced = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                      my disk 10 4 6 40% /mnt/my share\n";
        assert_eq!(parse_df(spaced), Some(6 * 1024));
        assert_eq!(parse_df("Filesystem\n"), None);
        assert_eq!(parse_df(""), None);
    }

    #[test]
    fn starts_are_seeded_and_cover_the_bottom() {
        let a = starts(0);
        assert_eq!(a.len(), SEGMENTS);
        assert!(a[0].is_empty());
        assert!(a[1..].iter().all(|k| k.len() == 20));
        assert_eq!(a, starts(0));
        assert_ne!(a, starts(1));
    }

    #[test]
    fn untrusted_text_is_escaped() {
        assert_eq!(shown(b"ok\x1b[2Jname\n"), "ok\\u{1b}[2Jname\\n");
        assert_eq!(shown(b"caf\xe9"), "caf\u{fffd}");
        let mut o = String::new();
        json_str(&mut o, "a\"b\\c\u{1}\u{7f}");
        assert_eq!(o, r#""a\"b\\c\u0001\u007f""#);
    }

    #[test]
    fn json_is_stable() {
        let mut r = Report::default();
        r.check("encoding", Status::Fail, "UTF8");
        r.samples.push(Sample {
            table: "torrents",
            column: "name",
            rows: 2,
            nulls: 0,
            utf8_valid: 1,
            non_ascii: 1,
            bytes: 9,
            sha256: "ab".into(),
        });
        assert_eq!(
            to_json(&r),
            "{\"format\":1,\"ok\":false,\"checks\":[{\"name\":\"encoding\",\"status\":\"fail\",\
             \"detail\":\"UTF8\"}],\"warnings\":[],\"server\":{\"version_num\":null,\"version\":null},\
             \"database\":{\"encoding\":null,\"collate\":null,\"ctype\":null,\"size_bytes\":null},\
             \"schema\":null,\"tables\":[],\"samples\":[{\"table\":\"torrents\",\"column\":\"name\",\
             \"rows\":2,\"nulls\":0,\"utf8_valid\":1,\"non_ascii\":1,\"bytes\":9,\"sha256\":\"ab\"}],\
             \"disk\":null}"
        );
    }
}
