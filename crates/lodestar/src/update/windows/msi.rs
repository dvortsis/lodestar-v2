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
//!   (Q13 rule 3): each is size-capped and parsed as untrusted input.
//! - **The staging folder** ([`Staging`]): `%ProgramFiles%\Lodestar\updates`,
//!   created by the helper with a protected ACL that lets in SYSTEM and
//!   Administrators only, and checked before every use (rules 1 and 2).
//! - **`msiexec`'s arguments are fixed** ([`msiexec_args`], rule 6).
//!
//! The helper's steps are in [`super::helper`].

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
pub const REQUEST: &str = "pending";
/// The signed envelope the service checked, as fetched.
pub const ENVELOPE: &str = "manifest.json";
/// The largest request read.
pub const MAX_REQUEST: u64 = 4 * 1024;
/// The request's format.
pub const REQUEST_FORMAT: u32 = 1;

/// `<data>\updates\pending`: the version the service asks for. Every
/// field is checked again against the signed manifest; the channel must
/// be the one the manifest is signed for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub format: u32,
    pub version: String,
    pub channel: Channel,
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
    /// The service didn't stop, or `.lock` stayed held.
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
    /// Not installed: Windows Installer rolled the attempt back.
    Failed(i32),
}

pub fn installed(code: i32) -> Installed {
    match code {
        0 => Installed::Done,
        3010 | 1641 => Installed::RestartNeeded,
        c => Installed::Failed(c),
    }
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

/// Renames `from` to `to`, retrying for about 1.3 s while Windows
/// reports a sharing conflict or a denied access: antivirus and the
/// search indexer hold a new file open for a moment (architecture B5a),
/// as `lodestar-search`'s index renames allow for. Elsewhere it's one
/// attempt.
pub fn rename_retrying(from: &Path, to: &Path) -> io::Result<()> {
    retry(sharing_conflict, || std::fs::rename(from, to))
}

/// A sharing conflict or denied access (os errors 32 and 5), Windows
/// only.
fn sharing_conflict(e: &io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32))
}

const ATTEMPTS: u32 = 8;
const FIRST_PAUSE: Duration = Duration::from_millis(5);
const MAX_PAUSE: Duration = Duration::from_millis(320);

fn retry(
    transient: fn(&io::Error) -> bool,
    mut op: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut pause = FIRST_PAUSE;
    let mut attempt = 1;
    loop {
        match op() {
            Err(e) if attempt < ATTEMPTS && transient(&e) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(MAX_PAUSE);
                attempt += 1;
            }
            result => return result,
        }
    }
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
      <Command>{exe}</Command>
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

    const SID: &str = "S-1-5-80-1-2-3-4-5";

    #[test]
    fn a_request_is_untrusted_input() {
        let ok = br#"{"format":1,"version":"0.7.0","channel":"stable"}"#;
        let (r, v) = parse_request(ok).unwrap();
        assert_eq!((r.channel, v), (Channel::Stable, Version::new(0, 7, 0)));
        let refused = |b: &[u8]| parse_request(b).unwrap_err().code();
        assert_eq!(
            refused(br#"{"format":1,"version":"0.7.0","channel":"stable","x":1}"#),
            "request_unreadable"
        );
        assert_eq!(
            refused(br#"{"format":2,"version":"0.7.0","channel":"stable"}"#),
            "request_format"
        );
        for bad in ["../x", "0.7.0+evil", "v0.7.0", "0.7", "00.7.0"] {
            let b = format!(r#"{{"format":1,"version":"{bad}","channel":"stable"}}"#);
            assert_eq!(refused(b.as_bytes()), "request_version", "{bad}");
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
        assert!(xml.contains(r"<Command>C:\Program Files\Lodestar\lodestar.exe</Command>"));
        assert!(xml.contains("<Arguments>update-helper</Arguments>"));
        // The service may read and start it, nothing more.
        assert!(xml.contains(
            "<SecurityDescriptor>D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FRFX;;;S-1-5-80-1-2-3-4-5)</SecurityDescriptor>"
        ));
        assert!(task_xml(Path::new(r"C:\x\lodestar.exe"), "S-1-5-18").is_err());
        assert!(task_xml(Path::new(r"C:\x\lodestar.exe"), "S-1-5-80-1)(A;;FA;;;WD").is_err());
        assert!(task_xml(Path::new(r"C:\a&b\lodestar.exe"), SID).is_err());
    }

    #[test]
    fn a_rename_retries_only_a_windows_sharing_conflict() {
        let mut n = 0;
        let r = retry(
            |e| e.raw_os_error() == Some(32),
            || {
                n += 1;
                if n < 3 {
                    Err(io::Error::from_raw_os_error(32))
                } else {
                    Ok(())
                }
            },
        );
        assert!(r.is_ok());
        assert_eq!(n, 3);
        // Bounded: the last error comes back.
        let mut n = 0;
        let r = retry(
            |_| true,
            || {
                n += 1;
                Err(io::Error::from_raw_os_error(32))
            },
        );
        assert_eq!(r.unwrap_err().raw_os_error(), Some(32));
        assert_eq!(n, ATTEMPTS);
        // Anything else isn't retried.
        let mut n = 0;
        let _ = retry(sharing_conflict, || {
            n += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(n, 1);
        assert_eq!(
            sharing_conflict(&io::Error::from_raw_os_error(32)),
            cfg!(windows)
        );
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
                "Highest|1|MSFT_TaskBootTrigger|1|{}|update-helper|IgnoreNew",
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
}
