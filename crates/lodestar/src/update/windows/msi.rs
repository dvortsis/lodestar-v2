//! The MSI's update helper, its shared and pure parts (PK-win-upd-2; plan
//! `docs/plan/2026-11-06-m6-gap-audit.md` §5 Q13, which the owner
//! approved in `docs/owner-decisions.md` §2.47):
//!
//! - **The task:** `Lodestar update helper`, a SYSTEM scheduled task the
//!   MSI registers ([`task_xml`]). It runs only `lodestar.exe
//!   update-helper` from `%ProgramFiles%\Lodestar`, on demand and at
//!   boot, and only SYSTEM and Administrators may change it. The
//!   service's own account may read and start it, nothing more.
//! - **What the service leaves for it** in `<data>\updates\`: the request
//!   ([`REQUEST`], a [`Request`]), the signed envelope it checked
//!   ([`ENVELOPE`]) and the downloaded MSI ([`msi_name`]). The service
//!   account can write all of them, so the helper trusts none of them
//!   (Q13 rule 3): each is size-capped and parsed as untrusted input. The
//!   helper deletes nothing there; it answers in [`RESULT`] (a
//!   [`Answer`]), and the service cleans the folder.
//! - **The staging folder** ([`Staging`]): `%ProgramFiles%\Lodestar\updates`,
//!   created by the helper with a protected ACL that lets in SYSTEM and
//!   Administrators only, and checked before every use (rules 1 and 2).
//! - **`msiexec`'s arguments are fixed** ([`msiexec_args`], rule 6).
//!
//! The helper's steps are in [`super::helper`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use std::io;

use lodestar_update::Channel;
use lodestar_update::semver::Version;
use serde::{Deserialize, Serialize};

/// The scheduled task's name (`main.wxs` registers it).
pub const TASK: &str = "Lodestar update helper";
/// The command the task runs, the one argument after the program.
pub const COMMAND: &str = "update-helper";
/// `<data>\updates\`: what the service leaves for the helper.
pub const UPDATES: &str = "updates";
/// The service's request: which version to install, from which channel.
/// Not Unix's `pending` (a probation record, which PK-win-upd-3 keeps in
/// `<data>\bin`).
pub const REQUEST: &str = "request.json";
/// The helper's answer to a request.
pub const RESULT: &str = "result.json";
/// In staging: the last request the helper handled, so a request is
/// acted on once (a boot run doesn't repeat it).
pub const HANDLED: &str = "handled.json";
/// The signed envelope the service checked, as fetched.
pub const ENVELOPE: &str = "manifest.json";
/// The largest request read.
pub const MAX_REQUEST: u64 = 4 * 1024;
/// The request's format.
pub const REQUEST_FORMAT: u32 = 1;

/// `<data>\updates\request.json`: the version the service asks for.
/// Every field is checked again against the signed manifest; the channel
/// must be the one the manifest is signed for. `id` is new for each
/// Install (16 to 64 lowercase hex digits): the helper acts on an id
/// once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub format: u32,
    pub id: String,
    pub version: String,
    pub channel: Channel,
}

/// `<data>\updates\result.json`: what became of request `id`.
/// `outcome` is [`super::helper::Outcome::code`] or, refused,
/// `"refused"` with the refusal's code in `code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub format: u32,
    pub id: String,
    pub version: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Windows must restart to finish replacing an in-use file.
    #[serde(default)]
    pub restart: bool,
    /// After a halted rollback: what an administrator does to recover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub at: u64,
}

/// Whether `id` is a request id.
pub fn valid_id(id: &str) -> bool {
    (16..=64).contains(&id.len()) && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Why the helper installs nothing. Each has a stable code for its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// This build's version doesn't parse, so "newer" means nothing.
    BuildVersion,
    /// The helper isn't `%ProgramFiles%\Lodestar\lodestar.exe`.
    NotInstalled,
    /// The data folder isn't the checked default, or isn't private.
    UncheckedBin(String),
    /// The staging folder couldn't be made private, or isn't.
    Staging(String),
    /// There's no request, or it doesn't parse.
    Request(&'static str),
    /// The envelope doesn't verify (its `lodestar_update::Error` code).
    Manifest(&'static str),
    /// The requested version isn't newer than the installed one.
    NotNewer,
    /// The release has no MSI for this build's target.
    NoArtifact,
    /// The MSI's copy isn't the pinned size or hash.
    Mismatch,
    /// The MSI carries an Authenticode signature that doesn't verify.
    Authenticode(String),
    /// The service didn't stop, `.lock` stayed held, or Windows Installer
    /// is busy with another install.
    Busy(&'static str),
    /// A file couldn't be read or written.
    Io(String),
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::BuildVersion => "build_version_unknown",
            Refusal::NotInstalled => "not_installed",
            Refusal::UncheckedBin(_) => "bin_unchecked",
            Refusal::Staging(_) => "staging_not_private",
            Refusal::Request(c) | Refusal::Manifest(c) | Refusal::Busy(c) => c,
            Refusal::NotNewer => "not_newer",
            Refusal::NoArtifact => "no_artifact",
            Refusal::Mismatch => "artifact_mismatch",
            Refusal::Authenticode(_) => "authenticode",
            Refusal::Io(_) => "io",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::UncheckedBin(why)
            | Refusal::Staging(why)
            | Refusal::Authenticode(why)
            | Refusal::Io(why) => write!(f, "{}: {why}", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}

/// Parses the request: capped, unknown fields refused, a known format, a
/// canonical version.
pub fn parse_request(bytes: &[u8]) -> Result<(Request, Version), Refusal> {
    if bytes.len() as u64 > MAX_REQUEST {
        return Err(Refusal::Request("request_too_large"));
    }
    let r: Request =
        serde_json::from_slice(bytes).map_err(|_| Refusal::Request("request_unreadable"))?;
    if r.format != REQUEST_FORMAT {
        return Err(Refusal::Request("request_format"));
    }
    if !valid_id(&r.id) {
        return Err(Refusal::Request("request_id"));
    }
    let v = super::super::layout::parse_version(&r.version)
        .filter(|v| v.to_string() == r.version)
        .ok_or(Refusal::Request("request_version"))?;
    Ok((r, v))
}

/// The MSI's file name for `version`, in `<data>\updates\` and in
/// staging. The version is canonical ([`parse_request`]), never a path.
pub fn msi_name(version: &Version) -> String {
    format!("lodestar-{version}.msi")
}

/// Rule 4: only a version newer than the installed one. RC and nightly
/// MSIs share their release's `ProductVersion`, so Windows Installer
/// can't tell them apart; the semantic version decides here, and
/// `main.wxs`'s `MajorUpgrade` takes a same-`ProductVersion` upgrade.
/// The journal's rollback target (PK-win-upd-3) is the only exception,
/// and doesn't come through here.
pub fn newer(running: &Version, wanted: &Version) -> bool {
    wanted > running
}

/// `msiexec`'s arguments, fixed (rule 6): install `msi` quietly, never
/// restart Windows, log verbosely to `log`, and don't let Restart
/// Manager close the helper, which runs from the folder being upgraded
/// (an in-use file is replaced at the next restart instead, exit 3010).
/// No public property is passed, so an upgrade keeps the options chosen
/// at install (`main.wxs` reads them back).
pub fn msiexec_args(msi: &Path, log: &Path) -> Vec<OsString> {
    vec![
        "/i".into(),
        msi.as_os_str().to_owned(),
        "/qn".into(),
        "/norestart".into(),
        "/l*v".into(),
        log.as_os_str().to_owned(),
        "MSIRESTARTMANAGERCONTROL=Disable".into(),
    ]
}

/// What `msiexec`'s exit code means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Done,
    /// Installed; a restart finishes it (3010, 1641).
    RestartNeeded,
    /// Not installed, and not this MSI's fault: another install is running
    /// (1618) or Windows Installer couldn't be reached (1601). The
    /// version isn't skipped; a later Install tries again.
    Busy(i32),
    /// Not installed: Windows Installer rolled the attempt back. The
    /// version is skipped as `failed`.
    Failed(i32),
}

pub fn installed(code: i32) -> Installed {
    match code {
        0 => Installed::Done,
        3010 | 1641 => Installed::RestartNeeded,
        1601 | 1618 => Installed::Busy(code),
        c => Installed::Failed(c),
    }
}

/// How much of the end of an `msiexec` log is read to tell why it failed
/// ([`held_open`]): StartServices runs near the end, and a verbose log can
/// run to megabytes.
pub const LOG_TAIL: u64 = 1024 * 1024;

/// Attempts in a row that may fail the way [`held_open`] recognises: the
/// last of them counts as an ordinary failure (the install's version is
/// skipped; the rollback's reinstall halts); each before it is tried again
/// at the helper's next run.
pub const MAX_BLOCKED: u32 = 3;

/// Whether an `msiexec` log's tail shows error 1920 for this service: the
/// MSI's StartServices couldn't start it. That's what a `lodestar.exe`
/// held open with no sharing gives (the SCM can't open the image), and the
/// install then fails 1603 and rolls back. It also covers a service that
/// fails to start for any other reason, which [`MAX_BLOCKED`] bounds.
///
/// The log is untrusted input: only `tail` (at most [`LOG_TAIL`] bytes,
/// the caller's read) is looked at, decoded as UTF-16LE when it has
/// Windows Installer's byte-order mark or its NUL pattern, otherwise as
/// lossy UTF-8. The pattern is fixed, and doesn't depend on Windows'
/// language: on one line, the number 1920 followed by `.` and a space,
/// right after a word, and then the service's name as a whole word
/// (`Error 1920. Service 'Lodestar' ...`; `Fehler 1920. Der Dienst
/// "Lodestar" ...`).
pub fn held_open(tail: &[u8], service: &str) -> bool {
    let text = decode_log(tail);
    text.lines().any(|line| error_1920(line, service))
}

/// `<letter> 1920. ` in `line`, with `service` as a whole word after it.
/// Linear in the line: the service's last whole-word place is found in one
/// pass, then each `1920. ` is a constant-time check against it (an
/// adversarial line of repeated `a 1920. ` isn't quadratic).
fn error_1920(line: &str, service: &str) -> bool {
    const AT: &str = " 1920. ";
    let Some(last) = last_word(line, service) else {
        return false;
    };
    line.match_indices(AT).any(|(at, _)| {
        let word = line[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_alphabetic);
        word && last >= at + AT.len()
    })
}

/// Where `name` last appears in `line` as a whole word: neither the
/// character before it nor the one after it is a letter, digit, `_` or `-`
/// (`'Lodestar'`, `"Lodestar"`, `(Lodestar)`, `Lodestar.`; not
/// `Lodestar2` or `Lodestar-old`).
fn last_word(line: &str, name: &str) -> Option<usize> {
    let inner = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-');
    line.match_indices(name)
        .filter(|(at, _)| {
            !inner(line[..*at].chars().next_back())
                && !inner(line[at + name.len()..].chars().next())
        })
        .map(|(at, _)| at)
        .last()
}

/// A log's bytes as text: UTF-16LE (Windows Installer's own, with a BOM,
/// or every other byte NUL) or else UTF-8, lossily. A tail may start
/// mid-character: an odd leading byte is dropped.
fn decode_log(bytes: &[u8]) -> String {
    let bom = bytes.starts_with(&[0xFF, 0xFE]);
    let nuls = bytes.len() >= 4 && bytes.iter().skip(1).step_by(2).take(64).all(|b| *b == 0);
    let odd_nuls = bytes.len() >= 4 && bytes.iter().step_by(2).take(64).all(|b| *b == 0);
    if bom || nuls || odd_nuls {
        let start = if bom {
            2
        } else if odd_nuls {
            1
        } else {
            0
        };
        let units: Vec<u16> = bytes[start..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// `%ProgramFiles%`, from the environment, else from the registry's
/// `ProgramFilesDir` (a service's environment has it, but a missing one
/// would refuse every MSI install, since the MSI is detected by its
/// folder). `registry` is `reg query`'s output, read only when needed.
pub fn program_files(
    env: Option<OsString>,
    registry: impl FnOnce() -> Option<String>,
) -> Option<PathBuf> {
    env.filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| parse_reg_value(&registry()?, "ProgramFilesDir").map(PathBuf::from))
}

/// The value of `name` in `reg query … /v name` output (`name REG_SZ
/// value`; the type and value aren't translated).
pub fn parse_reg_value(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let mut parts = l.trim().splitn(3, char::is_whitespace);
        if !parts.next()?.eq_ignore_ascii_case(name) {
            return None;
        }
        let rest = parts.collect::<Vec<_>>().join(" ");
        let value = rest.trim_start().strip_prefix("REG_SZ")?.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// `%ProgramFiles%`, as [`program_files`] finds it on this system.
#[cfg(windows)]
pub fn system_program_files() -> Option<PathBuf> {
    program_files(std::env::var_os("ProgramFiles"), || {
        let root = std::env::var_os("SystemRoot")?;
        let out = std::process::Command::new(Path::new(&root).join(r"System32\reg.exe"))
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion",
                "/v",
                "ProgramFilesDir",
            ])
            .env_remove(crate::windows::relaunch::MARKER)
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    })
}

/// The staging folder and what it holds (rules 1 and 2). Only the
/// helper writes here, as SYSTEM.
///
/// - `incoming\`: copies out of `<data>\updates\`, emptied before each
///   run; a copy is verified here and installed from here.
/// - `trust.json`: the helper's own trust state, so a validly signed
///   older manifest is refused even when the data folder's is rolled
///   back.
/// - `current\` and `previous\`: the MSI installed last and the one
///   before it, each with its pins (`pins.json`), for PK-win-upd-3's
///   rollback.
#[derive(Debug, Clone)]
pub struct Staging {
    pub dir: PathBuf,
}

impl Staging {
    /// `<install folder>\updates`.
    pub fn of(install_dir: &Path) -> Staging {
        Staging {
            dir: install_dir.join(UPDATES),
        }
    }
    pub fn incoming(&self) -> PathBuf {
        self.dir.join("incoming")
    }
    pub fn current(&self) -> PathBuf {
        self.dir.join("current")
    }
    pub fn previous(&self) -> PathBuf {
        self.dir.join("previous")
    }
    pub fn log(&self) -> PathBuf {
        self.dir.join("msiexec.log")
    }
}

/// The pins kept beside a staged MSI: what it was verified against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pins {
    pub version: String,
    pub channel: Channel,
    pub sha256: String,
    pub size: u64,
}

pub const PINS: &str = "pins.json";

/// The service's side of an MSI install: where the MSI comes from, and
/// how the helper's task is started (tests replace both).
pub struct Installer {
    pub source: std::sync::Arc<dyn crate::update::download::Source>,
    pub start: Box<dyn Fn() -> io::Result<()> + Send + Sync>,
}

impl Installer {
    /// The real one: the guarded download client, and `schtasks /Run`.
    #[cfg(windows)]
    pub fn system() -> Option<Installer> {
        let source = crate::update::download::Guarded::new().ok()?;
        Some(Installer {
            source: std::sync::Arc::new(source),
            start: Box::new(start_task),
        })
    }
}

/// Starts the task (`schtasks /Run`), the one thing the service may do
/// with it.
#[cfg(windows)]
pub fn start_task() -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    let root =
        std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("SystemRoot isn't set"))?;
    let mut cmd = std::process::Command::new(Path::new(&root).join(r"System32\schtasks.exe"));
    super::helper::clean_env(&mut cmd);
    let out = cmd
        .creation_flags(0x0800_0000)
        .args(["/Run", "/TN", TASK])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other("schtasks /Run failed"))
    }
}

/// What the service leaves for the helper, in `<data>\updates\`, once the
/// MSI is downloaded (to `part`, in `<data>\bin`, verified against the
/// pin): the envelope it checked, the MSI, then the request (a new id),
/// so the helper never sees a request without its files. Earlier files
/// (an answered request, an old MSI) go first. Returns the request's id.
pub fn hand_over(
    data_dir: &Path,
    part: &Path,
    envelope: &[u8],
    version: &Version,
    channel: Channel,
) -> io::Result<String> {
    use crate::update::layout;
    let updates = data_dir.join(UPDATES);
    match std::fs::create_dir(&updates) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let _pin = layout::pin_dir(&updates)?;
    clean(&updates);
    layout::write_atomic(&updates, ENVELOPE, envelope)?;
    layout::rename_retrying(part, &updates.join(msi_name(version)))?;
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|_| io::Error::other("no randomness"))?;
    let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let request = Request {
        format: REQUEST_FORMAT,
        id: id.clone(),
        version: version.to_string(),
        channel,
    };
    let bytes = serde_json::to_vec(&request).map_err(io::Error::other)?;
    layout::write_atomic(&updates, REQUEST, &bytes)?;
    Ok(id)
}

/// Removes what an earlier install left in `updates\`: the request, its
/// answer, the envelope and any MSI. Best effort.
pub fn clean(updates: &Path) {
    for name in [REQUEST, RESULT, ENVELOPE] {
        let _ = std::fs::remove_file(updates.join(name));
    }
    if let Ok(entries) = std::fs::read_dir(updates) {
        for e in entries.flatten() {
            let msi = e
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("lodestar-") && n.ends_with(".msi"));
            if msi && e.file_type().is_ok_and(|t| t.is_file()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// The helper's answer, when it's there and readable (it's display
/// only: the service can write the file too).
pub fn answer(data_dir: &Path) -> Option<Answer> {
    crate::update::layout::read_json(&data_dir.join(UPDATES).join(RESULT))
        .ok()
        .flatten()
}

/// The task's definition (Task Scheduler's XML). `exe` is
/// `%ProgramFiles%\Lodestar\lodestar.exe`; `service_sid` is
/// `NT SERVICE\Lodestar`'s SID.
///
/// - **Runs as** SYSTEM (`S-1-5-18`), with its highest rights.
/// - **When:** at boot (the one trigger) and on demand. One instance at
///   a time: a second start while one runs is ignored. Two hours at most.
/// - **Who may change or start it:** the task's own descriptor allows
///   SYSTEM and Administrators everything, and the service's SID only
///   read and execute (start it), nothing inherited.
/// - **What it runs:** `exe update-helper`, nothing else.
pub fn task_xml(exe: &Path, service_sid: &str) -> Result<String, &'static str> {
    if !service_sid.starts_with("S-1-5-80-")
        || !service_sid[9..]
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-')
    {
        return Err("not a service SID");
    }
    let exe = exe.to_str().ok_or("the program's path isn't Unicode")?;
    if exe.contains(['<', '>', '&', '"', '\'']) {
        return Err("the program's path can't be written in the task");
    }
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>Lodestar</Author>
    <Description>Installs a Lodestar update the Lodestar service downloaded, after checking it.</Description>
    <SecurityDescriptor>{sd}</SecurityDescriptor>
  </RegistrationInfo>
  <Triggers>
    <BootTrigger>
      <Enabled>true</Enabled>
    </BootTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>S-1-5-18</UserId>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT2H</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
      <Arguments>{COMMAND}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        sd = task_sddl(service_sid),
    ))
}

/// The task's descriptor: protected, SYSTEM and Administrators full
/// access, the service read and execute.
pub fn task_sddl(service_sid: &str) -> String {
    format!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FRFX;;;{service_sid})")
}

/// Registers [`task_xml`] (replacing an older definition) through
/// PowerShell, reading the XML from stdin so nothing in it is parsed as
/// script. Run by the MSI as SYSTEM.
pub const REGISTER_SCRIPT: &str = "$ErrorActionPreference = 'Stop'
$x = [Console]::In.ReadToEnd()
$null = Register-ScheduledTask -TaskName $env:LODESTAR_TASK -Xml $x -Force";

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str, bom: bool) -> Vec<u8> {
        let mut b = if bom { vec![0xFF, 0xFE] } else { Vec::new() };
        for u in text.encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b
    }

    const LOG_1920: &str = "MSI (s) (A8:B4) [10:15:23:456]: Executing op: ServiceControl(,Name=Lodestar,Action=1,Wait=1,)\r\n\
        MSI (s) (A8:B4) [10:15:53:456]: Product: Lodestar -- Error 1920. Service 'Lodestar' (Lodestar) failed to start. Verify that you have sufficient privileges to start system services.\r\n\
        MSI (s) (A8:B4) [10:15:54:000]: Product: Lodestar -- Installation failed.\r\n";

    /// Error 1920 for this service, in Windows Installer's UTF-16 (with or
    /// without its byte-order mark, a tail starting on either byte), in
    /// UTF-8, and in another language.
    #[test]
    fn a_1920_for_the_service_is_recognised() {
        assert!(held_open(&utf16(LOG_1920, true), "Lodestar"));
        assert!(held_open(&utf16(LOG_1920, false), "Lodestar"));
        assert!(
            held_open(&utf16(LOG_1920, false)[1..], "Lodestar"),
            "odd start"
        );
        assert!(held_open(LOG_1920.as_bytes(), "Lodestar"));
        let german = "MSI (s) (A8:B4) [10:15:53:456]: Produkt: Lodestar -- Fehler 1920. Der Dienst \"Lodestar\" (Lodestar) konnte nicht gestartet werden.";
        assert!(held_open(&utf16(german, true), "Lodestar"));
    }

    /// The service's name is matched as a whole word: another service
    /// whose name starts or ends with it isn't this one.
    #[test]
    fn only_the_services_own_name_counts() {
        for other in [
            "Lodestar2",
            "MyLodestar",
            "Lodestar_old",
            "Lodestar-old",
            "old-Lodestar",
        ] {
            let log =
                format!("Product: X -- Error 1920. Service '{other}' ({other}) failed to start.");
            assert!(!held_open(log.as_bytes(), "Lodestar"), "{other}");
        }
        let log = "Product: X -- Error 1920. Service 'Lodestar2' (Lodestar) failed to start.";
        assert!(
            held_open(log.as_bytes(), "Lodestar"),
            "a whole word later on"
        );
    }

    /// #677's review: a line of repeated `a 1920. ` is answered in linear
    /// time, with the same answers as before: no name, the name at the
    /// end, the name only before every 1920. The tail is at most 1 MiB;
    /// the line here is 4 MiB so the deadline separates the two: the old
    /// quadratic matcher took ~6 s at 1 MiB (~100 s at 4 MiB) in debug,
    /// the linear one well under a second at 4 MiB.
    #[test]
    fn an_adversarial_line_is_linear() {
        let flood = "a 1920. ".repeat(4 * LOG_TAIL as usize / 8);
        let start = std::time::Instant::now();
        assert!(!held_open(flood.as_bytes(), "Lodestar"));
        assert!(held_open(format!("{flood}Lodestar").as_bytes(), "Lodestar"));
        let begun = format!("Lodestar {flood}");
        assert!(
            !held_open(begun.as_bytes(), "Lodestar"),
            "the name must come after"
        );
        let elapsed = start.elapsed();
        assert!(elapsed < std::time::Duration::from_secs(10), "{elapsed:?}");
    }

    proptest::proptest! {
        /// Any bytes at all: an answer, never a panic.
        #[test]
        fn any_log_bytes_are_safe(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
            let _ = held_open(&bytes, "Lodestar");
        }

        /// A 1920 line between any printable lines is found, in UTF-8 and in
        /// UTF-16 with its mark.
        #[test]
        fn a_1920_line_is_found_among_any_others(
            before in "[ -~\r\n]{0,512}",
            after in "[ -~\r\n]{0,512}",
        ) {
            let line = "Product: Lodestar -- Error 1920. Service 'Lodestar' failed to start.";
            let text = format!("{before}\n{line}\n{after}");
            proptest::prop_assert!(held_open(text.as_bytes(), "Lodestar"));
            proptest::prop_assert!(held_open(&utf16(&text, true), "Lodestar"));
        }
    }

    /// Anything else isn't: another error, 1920 for another service, the
    /// number inside another one or without its word, an empty or
    /// garbled log.
    #[test]
    fn anything_else_is_an_ordinary_failure() {
        for log in [
            "MSI (s): Product: Lodestar -- Error 1603. A fatal error occurred during installation.",
            "MSI (s): Product: Other -- Error 1920. Service 'Spooler' (Print Spooler) failed to start.",
            "MSI (s): Lodestar [11:19:20:011] 11920. 1920. done",
            "MSI (s): Lodestar: Note: 1: 2205 2: 3: Error 4: SELECT 1920. ",
            "MSI (s): Note: 1: 2205 2: 1920. 3: ServiceControl Lodestar",
            "MSI (s) [10:15:53]: Property(S): COUNT = 1920. Lodestar",
            "",
        ] {
            assert!(!held_open(log.as_bytes(), "Lodestar"), "{log}");
            assert!(!held_open(&utf16(log, true), "Lodestar"), "{log}");
        }
        assert!(!held_open(&[0xFF, 0xFE, 0x00], "Lodestar"));
        assert!(!held_open(&[0xC3, 0x28, 0xFF], "Lodestar"));
    }

    const SID: &str = "S-1-5-80-1-2-3-4-5";

    #[test]
    fn a_request_is_untrusted_input() {
        let ok = br#"{"format":1,"id":"0123456789abcdef","version":"0.7.0","channel":"stable"}"#;
        let (r, v) = parse_request(ok).unwrap();
        assert_eq!((r.channel, v), (Channel::Stable, Version::new(0, 7, 0)));
        let refused = |b: &[u8]| parse_request(b).unwrap_err().code();
        assert_eq!(
            refused(br#"{"format":1,"id":"0123456789abcdef","version":"0.7.0","channel":"stable","x":1}"#),
            "request_unreadable"
        );
        assert_eq!(
            refused(
                br#"{"format":2,"id":"0123456789abcdef","version":"0.7.0","channel":"stable"}"#
            ),
            "request_format"
        );
        for bad in ["../x", "0.7.0+evil", "v0.7.0", "0.7", "00.7.0"] {
            let b = format!(
                r#"{{"format":1,"id":"0123456789abcdef","version":"{bad}","channel":"stable"}}"#
            );
            assert_eq!(refused(b.as_bytes()), "request_version", "{bad}");
        }
        for bad in [
            "",
            "0123",
            "0123456789ABCDEF",
            "0123456789abcdeg",
            "../0123456789abcdef",
        ] {
            let b = format!(r#"{{"format":1,"id":"{bad}","version":"0.7.0","channel":"stable"}}"#);
            assert_eq!(refused(b.as_bytes()), "request_id", "{bad}");
        }
        assert_eq!(refused(&vec![b' '; 5000]), "request_too_large");
        assert_eq!(refused(b"\xff"), "request_unreadable");
    }

    #[test]
    fn only_a_newer_version_installs() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(newer(&v("0.6.0"), &v("0.7.0")));
        assert!(newer(&v("0.7.0-rc.1"), &v("0.7.0")));
        assert!(!newer(&v("0.7.0"), &v("0.7.0")));
        assert!(!newer(&v("0.7.0"), &v("0.7.0-rc.2")));
        assert!(!newer(&v("0.7.0"), &v("0.6.9")));
    }

    #[test]
    fn msiexec_arguments_are_fixed() {
        let args = msiexec_args(Path::new(r"C:\s\i.msi"), Path::new(r"C:\s\m.log"));
        assert_eq!(
            args,
            [
                "/i",
                r"C:\s\i.msi",
                "/qn",
                "/norestart",
                "/l*v",
                r"C:\s\m.log",
                "MSIRESTARTMANAGERCONTROL=Disable"
            ]
            .map(OsString::from)
        );
        assert_eq!(installed(0), Installed::Done);
        assert_eq!(installed(3010), Installed::RestartNeeded);
        assert_eq!(installed(1641), Installed::RestartNeeded);
        assert_eq!(installed(1603), Installed::Failed(1603));
        assert_eq!(installed(1638), Installed::Failed(1638));
        assert_eq!(installed(1618), Installed::Busy(1618));
        assert_eq!(installed(1601), Installed::Busy(1601));
    }

    #[test]
    fn program_files_falls_back_to_the_registry() {
        let reg = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\r\n    ProgramFilesDir    REG_SZ    D:\\Program Files\r\n\r\n";
        assert_eq!(
            program_files(Some(r"C:\Program Files".into()), || panic!("not read")),
            Some(PathBuf::from(r"C:\Program Files"))
        );
        assert_eq!(
            program_files(None, || Some(reg.into())),
            Some(PathBuf::from(r"D:\Program Files"))
        );
        assert_eq!(
            program_files(Some(OsString::new()), || Some(reg.into())),
            Some(PathBuf::from(r"D:\Program Files"))
        );
        assert_eq!(program_files(None, || None), None);
        assert_eq!(program_files(None, || Some("ERROR: nothing".into())), None);
        assert_eq!(
            parse_reg_value("    ProgramFilesDir    REG_DWORD    0x1", "ProgramFilesDir"),
            None
        );
    }

    #[test]
    fn the_task_runs_only_the_helper_as_system_at_boot_or_on_demand() {
        let xml = task_xml(Path::new(r"C:\Program Files\Lodestar\lodestar.exe"), SID).unwrap();
        let count = |s: &str| xml.matches(s).count();
        assert_eq!(count("<UserId>S-1-5-18</UserId>"), 1);
        assert_eq!(count("<Principal "), 1);
        // One trigger, at boot; on demand is a setting.
        assert_eq!(count("Trigger>"), 2);
        assert_eq!(count("<BootTrigger>"), 1);
        assert_eq!(count("<AllowStartOnDemand>true</AllowStartOnDemand>"), 1);
        assert_eq!(count("<MultipleInstancesPolicy>IgnoreNew"), 1);
        // One action: the program, the one argument.
        assert_eq!(count("<Exec>"), 1);
        assert!(xml.contains(r#"<Command>"C:\Program Files\Lodestar\lodestar.exe"</Command>"#));
        assert!(xml.contains("<Arguments>update-helper</Arguments>"));
        // The service may read and start it, nothing more.
        assert!(xml.contains(
            "<SecurityDescriptor>D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FRFX;;;S-1-5-80-1-2-3-4-5)</SecurityDescriptor>"
        ));
        assert!(task_xml(Path::new(r"C:\x\lodestar.exe"), "S-1-5-18").is_err());
        assert!(task_xml(Path::new(r"C:\x\lodestar.exe"), "S-1-5-80-1)(A;;FA;;;WD").is_err());
        assert!(task_xml(Path::new(r"C:\a&b\lodestar.exe"), SID).is_err());
    }

    /// Task Scheduler takes the definition as written: registered under
    /// a test name, read back, then removed. Needs an administrator (CI's
    /// runner is one).
    #[cfg(windows)]
    #[test]
    fn task_scheduler_takes_the_definition() {
        if std::env::var_os("CI").is_none() && std::env::var_os("LODESTAR_SERVICE_TEST").is_none() {
            return;
        }
        use std::io::Write;
        // A service every Windows has stands in for Lodestar's.
        let sid = crate::windows::acl::service_sid("EventLog").unwrap();
        let exe = Path::new(r"C:\Program Files\Lodestar\lodestar.exe");
        let xml = task_xml(exe, &sid).unwrap();
        let name = format!("Lodestar update helper test {}", std::process::id());
        let ps = |script: &str, stdin: Option<&str>| {
            let mut child = std::process::Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    script,
                ])
                .env("LODESTAR_TASK", &name)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut input = child.stdin.take().unwrap();
            input.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
            drop(input);
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        ps(REGISTER_SCRIPT, Some(&xml));
        let back = ps(
            "$t = Get-ScheduledTask -TaskName $env:LODESTAR_TASK
\"$($t.Principal.RunLevel)|$($t.Triggers.Count)|$($t.Triggers[0].CimClass.CimClassName)|$($t.Actions.Count)|$($t.Actions[0].Execute)|$($t.Actions[0].Arguments)|$($t.Settings.MultipleInstances)\"
$t.Principal.UserId
$t.SecurityDescriptor",
            None,
        );
        ps(
            "Unregister-ScheduledTask -TaskName $env:LODESTAR_TASK -Confirm:$false",
            None,
        );
        let lines: Vec<&str> = back.lines().map(str::trim).collect();
        assert_eq!(
            lines.first().copied().unwrap_or_default(),
            format!(
                "Highest|1|MSFT_TaskBootTrigger|1|\"{}\"|update-helper|IgnoreNew",
                exe.display()
            ),
            "{back}"
        );
        let user = lines
            .get(1)
            .copied()
            .unwrap_or_default()
            .to_ascii_uppercase();
        assert!(
            ["SYSTEM", "NT AUTHORITY\\SYSTEM", "S-1-5-18"].contains(&user.as_str()),
            "{back}"
        );
        let sd = lines.get(2).copied().unwrap_or_default();
        assert!(sd.contains("D:P"), "{back}");
        assert!(sd.contains(&sid), "{back}");
        assert!(
            !sd.contains(";WD)") && !sd.contains(";AU)") && !sd.contains(";BU)"),
            "{back}"
        );
    }

    #[test]
    fn the_hand_over_leaves_the_files_then_the_request() {
        let data = tempfile::tempdir().unwrap();
        let updates = data.path().join(UPDATES);
        std::fs::create_dir(&updates).unwrap();
        // An earlier install's leftovers.
        for name in [RESULT, REQUEST, "lodestar-0.6.0.msi"] {
            std::fs::write(updates.join(name), b"old").unwrap();
        }
        let part = data.path().join("0.7.0.part");
        std::fs::write(&part, b"msi").unwrap();
        let v = Version::new(0, 7, 0);
        let id = hand_over(data.path(), &part, b"{envelope}", &v, Channel::Stable).unwrap();
        assert!(valid_id(&id), "{id}");
        assert!(!part.exists());
        let mut names: Vec<String> = std::fs::read_dir(&updates)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["lodestar-0.7.0.msi", ENVELOPE, REQUEST]);
        let (r, rv) = parse_request(&std::fs::read(updates.join(REQUEST)).unwrap()).unwrap();
        assert_eq!(
            (r.id, r.channel, rv),
            (id.clone(), Channel::Stable, v.clone())
        );
        // Each hand-over is a new request.
        std::fs::write(&part, b"msi").unwrap();
        let again = hand_over(data.path(), &part, b"{envelope}", &v, Channel::Stable).unwrap();
        assert_ne!(again, id);
        assert_eq!(answer(data.path()), None);
    }
}
