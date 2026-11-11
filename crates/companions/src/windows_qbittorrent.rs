//! qBittorrent on Windows: a guided install, then detection and
//! configuration as for any guided companion (owner cq4,
//! `docs/owner-decisions.md` §1.7.2's one exception; plan
//! 2026-11-06-v06-companions CMP-11, Q4; architecture §18.10).
//!
//! **Lodestar never downloads or runs the installer.** The only official
//! Windows build is an installer that needs an administrator and runs as a
//! desktop program; the service has neither. So the catalogue's Windows
//! artifact is an [`ArchiveKind::Installer`], which CMP-5's manager refuses
//! (`guided_only`) before anything is fetched, and this module only shows:
//! the official download page, the pinned version, its file name and
//! SHA-256 to check, and the steps ([`Guide`]). Nothing here holds an HTTP
//! client for the download, and nothing builds a command line.
//!
//! **Then the guided path takes over** (CMP-5g): detection finds the WebUI
//! on loopback (`http://localhost:8080`, the catalogue's `webui` port), you
//! paste the WebUI password you set, Lodestar replaces it with a generated
//! one through CMP-8b's adapter and hands it over to `integrations.clients`
//! as the managed download client.
//!
//! **One gate of its own:** the version. [`WindowsQBittorrent`] wraps
//! CMP-8b's qBittorrent adapter; signed in, it reads the version and
//! refuses one outside the pin's range ([`Supported`]) with
//! [`VERSION_UNSUPPORTED`] before anything is written. An unpinned
//! catalogue entry refuses everything (`awaiting_pin`).
//!
//! **Windows save paths** ([`WindowsPath`]) are the ones CMP-8b's
//! [`HostPath`](crate::inputs::HostPath) refused: a drive letter or a UNC
//! share, backslashes only, with device names, `\\?\` and `\\.\`, traversal
//! and Windows' own folders refused.
//!
//! **Credentials:** the password you paste reaches this module only inside
//! a [`Secret`](lodestar_integrations::secret::Secret) in the
//! [`ConfigureInput`]; it's sent only in CMP-8b's sign-in form body, never
//! in a URL, a log line, an error or `Debug`.

use std::fmt;

use crate::adapter::{
    AdapterError, BoxFuture, CheckedPlan, ConfigPlan, Configure, ConfigureInput, GuardedHttp,
    Target,
};
use crate::catalogue::{Arch, ArchiveKind, Entry, NotAvailable, Pin, Platform};
use crate::configure::qbittorrent;
use crate::inputs::{CompanionId, InputError, Version};

/// qBittorrent's own download page. The installer is linked from there;
/// Lodestar shows this and never fetches it.
pub const DOWNLOAD_PAGE: &str = "https://www.qbittorrent.org/download";

/// The refusal for a qBittorrent outside the pinned version's range.
pub const VERSION_UNSUPPORTED: &str = "qbittorrent_version_unsupported";
/// The refusal while the Windows installer awaits its pin (CMP-13).
pub const AWAITING_PIN: &str = "awaiting_pin";
/// The refusal when the catalogue has no Windows installer for it.
pub const NO_INSTALLER: &str = "no_platform";

// ─── The pin and its range ──────────────────────────────────────────────

/// The pinned Windows installer: what you download and check.
#[derive(Debug, Clone, Copy)]
pub struct Installer<'a> {
    pin: &'a Pin,
}

impl<'a> Installer<'a> {
    /// The entry's Windows x64 installer and its pin. An entry without one,
    /// or with a Windows build that isn't an installer, is
    /// [`NotAvailable::NoPlatform`]; an unpinned one
    /// [`NotAvailable::AwaitingPin`].
    pub fn of(entry: &'a Entry) -> Result<Installer<'a>, NotAvailable> {
        let a = entry
            .artifact(Platform::Windows, Arch::X86_64)
            .filter(|a| a.archive == ArchiveKind::Installer)
            .ok_or(NotAvailable::NoPlatform)?;
        Ok(Installer { pin: a.pin()? })
    }

    pub fn version(&self) -> &'a Version {
        &self.pin.version
    }

    /// The installer's file name, as the official page offers it.
    pub fn file_name(&self) -> &'a str {
        self.pin.url.file_name()
    }

    /// Its SHA-256, 64 lower-case hex digits, for you to compare.
    pub fn sha256(&self) -> String {
        self.pin.sha256.to_hex()
    }

    /// The versions Lodestar accepts from this pin.
    pub fn supported(&self) -> Supported {
        Supported::from_pin(self.pin.version.clone())
    }
}

/// The versions a pin accepts: the pinned one and later patch releases of
/// the same `major.minor` (`5.0.3` accepts `5.0.3` to `5.0.x`, not `5.0.2`
/// or `5.1.0`). An older one may lack the pin's fixes; a newer minor
/// release is one Lodestar hasn't checked CMP-8b's adapter against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supported {
    pinned: Version,
}

/// Why a version is outside [`Supported`]. Carries only Lodestar's own
/// parsed versions, so its message can be shown as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    Older { found: Version, pinned: Version },
    Newer { found: Version, pinned: Version },
}

impl Supported {
    pub fn from_pin(pinned: Version) -> Supported {
        Supported { pinned }
    }

    pub fn check(&self, found: &Version) -> Result<(), Unsupported> {
        let mm = |v: &Version| {
            let mut it = v.as_str().split('.').map(|p| p.parse::<u32>().unwrap_or(0));
            (it.next().unwrap_or(0), it.next().unwrap_or(0))
        };
        let out = |older| {
            let (found, pinned) = (found.clone(), self.pinned.clone());
            Err(if older {
                Unsupported::Older { found, pinned }
            } else {
                Unsupported::Newer { found, pinned }
            })
        };
        if *found < self.pinned {
            out(true)
        } else if mm(found) != mm(&self.pinned) {
            out(false)
        } else {
            Ok(())
        }
    }
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unsupported::Older { found, pinned } => write!(
                f,
                "This qBittorrent is {found}, older than {pinned}, the version Lodestar \
                 supports on Windows. Install {pinned} from qBittorrent's download page, then \
                 try again."
            ),
            Unsupported::Newer { found, pinned } => write!(
                f,
                "This qBittorrent is {found}, newer than the {pinned} release Lodestar has \
                 checked on Windows. Install {pinned} from qBittorrent's download page, or \
                 wait for a Lodestar update that supports {found}."
            ),
        }
    }
}

// ─── The steps shown ────────────────────────────────────────────────────

/// One step of the guided install, in the order shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Download the pinned installer from [`DOWNLOAD_PAGE`].
    Download,
    /// Compare its SHA-256 before running it.
    CheckSha256,
    /// Run the installer yourself (it asks for an administrator).
    Install,
    /// Turn on the WebUI, on this computer only, on its port.
    TurnOnWebUi,
    /// Set a WebUI user name and password; keep the localhost bypass off.
    SetPassword,
    /// Let Lodestar find it, and paste the password once.
    Connect,
}

impl Step {
    pub const ALL: [Step; 6] = [
        Step::Download,
        Step::CheckSha256,
        Step::Install,
        Step::TurnOnWebUi,
        Step::SetPassword,
        Step::Connect,
    ];

    /// A stable code for the UI's own messages.
    pub fn code(self) -> &'static str {
        match self {
            Step::Download => "download",
            Step::CheckSha256 => "check_sha256",
            Step::Install => "install",
            Step::TurnOnWebUi => "turn_on_webui",
            Step::SetPassword => "set_password",
            Step::Connect => "connect",
        }
    }
}

/// The pinned release the guide shows, once CMP-13 has pinned it.
#[derive(Debug, Clone)]
pub struct Shown {
    pub version: Version,
    pub file_name: String,
    /// 64 lower-case hex digits.
    pub sha256: String,
}

/// What the guided install shows: everything comes from the catalogue's pin
/// and Lodestar's own constants. Until the installer is pinned there's no
/// version or checksum to show, and none is claimed (runner, 2026-11-11):
/// the steps say to download from the official page.
#[derive(Debug, Clone)]
pub struct Guide {
    pub download_page: &'static str,
    pub pinned: Option<Shown>,
    /// The WebUI port detection looks on.
    pub port: u16,
}

impl Guide {
    /// The guide for `entry`'s Windows installer, pinned or not.
    /// [`NotAvailable::NoPlatform`] when it has none.
    pub fn of(entry: &Entry) -> Result<Guide, NotAvailable> {
        let pinned = match Installer::of(entry) {
            Ok(i) => Some(Shown {
                version: i.version().clone(),
                file_name: i.file_name().to_owned(),
                sha256: i.sha256(),
            }),
            Err(NotAvailable::AwaitingPin) => None,
            Err(e) => return Err(e),
        };
        let port = entry
            .health
            .as_ref()
            .and_then(|h| entry.port(h.port.as_str()))
            .map(|p| p.number.get())
            .ok_or(NotAvailable::NoPlatform)?;
        Ok(Guide {
            download_page: DOWNLOAD_PAGE,
            pinned,
            port,
        })
    }

    /// The steps, in order. The checksum step only with a pin.
    pub fn steps(&self) -> Vec<Step> {
        Step::ALL
            .into_iter()
            .filter(|s| *s != Step::CheckSha256 || self.pinned.is_some())
            .collect()
    }

    /// The WebUI address detection uses (`http://localhost:8080`).
    pub fn webui(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// A step's text, in English, filled from this guide. `None` for the
    /// checksum step without a pin.
    pub fn text(&self, step: Step) -> Option<String> {
        let page = self.download_page;
        let port = self.port;
        Some(match step {
            Step::Download => match &self.pinned {
                Some(p) => format!(
                    "Open {page} and download qBittorrent {} for Windows ({}). Lodestar \
                     doesn't download or run it for you.",
                    p.version, p.file_name
                ),
                None => format!(
                    "Open {page} and download qBittorrent's 64-bit Windows installer from \
                     there. Lodestar doesn't download or run it for you."
                ),
            },
            Step::CheckSha256 => {
                let p = self.pinned.as_ref()?;
                format!(
                    "Check the file before you run it. In PowerShell, in the folder you saved \
                     it to, run Get-FileHash .\\{} -Algorithm SHA256. The hash must be {}. \
                     If it isn't, delete the file and don't run it.",
                    p.file_name, p.sha256
                )
            }
            Step::Install => "Run the installer and follow its steps. Windows asks for an \
                 administrator's permission; that's the installer's, not Lodestar's."
                .to_owned(),
            Step::TurnOnWebUi => format!(
                "Start qBittorrent. Open Tools → Options → Web UI, tick Web User Interface \
                 (Remote control), set IP address to 127.0.0.1 so only this computer can \
                 reach it, and keep port {port}."
            ),
            Step::SetPassword => "On the same page, set a user name and a password you \
                 don't use anywhere else. Leave Bypass authentication for clients on \
                 localhost off, then select OK."
                .to_owned(),
            Step::Connect => format!(
                "Back in Lodestar, select Find companions. It finds qBittorrent at {}. Paste \
                 the password when asked: Lodestar replaces it with one it generates and \
                 makes qBittorrent your download client.",
                self.webui()
            ),
        })
    }
}

// ─── The configuration gate ─────────────────────────────────────────────

/// CMP-8b's qBittorrent adapter with the Windows version gate: signed in,
/// a version outside the pin's range is refused before anything is
/// written. Registered for qBittorrent on Windows
/// ([`crate::configure::configurator`]).
pub struct WindowsQBittorrent {
    inner: Box<dyn Configure>,
    entry: &'static Entry,
}

impl fmt::Debug for WindowsQBittorrent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsQBittorrent")
            .field("companion", &self.entry.id)
            .finish_non_exhaustive()
    }
}

impl WindowsQBittorrent {
    /// For `entry`, the catalogue's qBittorrent.
    pub fn new(entry: &'static Entry) -> WindowsQBittorrent {
        WindowsQBittorrent {
            inner: Box::new(qbittorrent::QBittorrent::new(entry.id.clone())),
            entry,
        }
    }

    /// Signs in and checks the version against the pin.
    async fn gate(
        &self,
        http: &dyn GuardedHttp,
        target: &Target,
        input: &ConfigureInput<'_>,
        temporary_first: bool,
    ) -> Result<(), AdapterError> {
        let supported = match Installer::of(self.entry) {
            Ok(i) => i.supported(),
            Err(NotAvailable::AwaitingPin) => return Err(AdapterError::Refused(AWAITING_PIN)),
            Err(_) => return Err(AdapterError::Refused(NO_INSTALLER)),
        };
        let found = qbittorrent::signed_in_version(http, target, input, temporary_first).await?;
        supported
            .check(&found)
            .map_err(|_| AdapterError::Refused(VERSION_UNSUPPORTED))
    }

    /// Why `found` is refused, as a plain message, or `None` when it's
    /// supported. For the screen's text beside [`VERSION_UNSUPPORTED`].
    pub fn explain(&self, found: &Version) -> Option<String> {
        let i = Installer::of(self.entry).ok()?;
        i.supported().check(found).err().map(|u| u.to_string())
    }
}

impl Configure for WindowsQBittorrent {
    fn companion(&self) -> &CompanionId {
        &self.entry.id
    }

    fn plan<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
    ) -> BoxFuture<'a, Result<ConfigPlan, AdapterError>> {
        Box::pin(async move {
            // CMP-8b's plan first: a wrong password then costs one refused
            // sign-in, as on any other platform.
            let plan = self.inner.plan(http, target, input).await?;
            self.gate(http, target, input, false).await?;
            Ok(plan)
        })
    }

    fn apply_plan<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
        plan: CheckedPlan<'a>,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async move {
            // The gate before any write: a qBittorrent changed between the
            // plan and the apply is checked again.
            let replaces = plan.plan().changes().contains(&qbittorrent::PASSWORD);
            self.gate(http, target, input, replaces).await?;
            self.inner.apply_plan(http, target, input, plan).await
        })
    }
}

/// The Windows configurator for qBittorrent, from the compiled-in
/// catalogue.
pub(crate) fn configurator(id: &CompanionId) -> Option<Box<dyn Configure>> {
    let entry = crate::builtin().ok()?.get(id.as_str())?;
    Some(Box::new(WindowsQBittorrent::new(entry)))
}

// ─── Windows save paths ─────────────────────────────────────────────────

/// The longest Windows path accepted, in bytes: `CreateDirectoryW`'s limit
/// without `\\?\` is `MAX_PATH - 12` (248) including the terminating null.
pub const WINDOWS_PATH_MAX: usize = 247;

/// Device names Windows opens in any folder and with any extension
/// (`C:\x\nul.txt` is the null device).
const DEVICES: &[&str] = &["con", "prn", "aux", "nul"];

/// Folders directly in a drive's root that a companion is never given,
/// with everything below them.
const FORBIDDEN_ROOTS: &[&str] = &[
    "windows",
    "program files",
    "programdata",
    "system volume information",
    "recovery",
    "boot",
    "perflogs",
];

/// A save path for qBittorrent on Windows, as qBittorrent itself sees it.
///
/// **Two shapes, backslashes only** (a `/` anywhere is refused rather than
/// guessed at, so no mixed spelling can name another folder):
/// - a drive path `X:\folder\…`: a letter, `:`, `\` and at least one
///   folder;
/// - a UNC share `\\server\share\…`: a host name or IPv4 address, then a
///   share name.
///
/// **Each folder name** is ASCII letters, digits, `-`, `_`, `.` and an
/// inner space; never empty, `.` or `..`; never ending in `.` or a space,
/// which Windows strips (`folder.` is `folder`, `.. ` is `..`). That set
/// already keeps out `:` (alternate data streams), `~` (8.3 short names
/// such as `PROGRA~1`), `$` (`C$`, `ADMIN$`), `%` (environment variables),
/// wildcards and anything outside ASCII (best-fit mappings).
///
/// **Refused** ([`InputError::Forbidden`] unless said):
/// - `\\?\` and `\\.\` (the verbatim and device namespaces), `\??\`, and
///   every relative form: `C:folder`, `\folder`, `folder`
///   ([`InputError::NotAbsolute`]);
/// - a device name as any folder, with or without an extension or spaces
///   before it: `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9`
///   ([`InputError::Reserved`]);
/// - a drive's root, Windows' own folders (`Windows`, `Program Files`,
///   `ProgramData`, `System Volume Information`, `Recovery`, `Boot`,
///   `PerfLogs`) and everything below them;
/// - `X:\Users` and `X:\Users\<name>` themselves, and a profile's
///   `AppData` or dot folders (`.ssh`) with everything below them.
///   `C:\Users\<name>\Downloads` is fine.
///
/// Compared without case, as Windows does. Lodestar's own data on Windows
/// is under `C:\ProgramData\Lodestar`, which the list keeps out.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WindowsPath(String);

impl WindowsPath {
    pub fn parse(s: &str) -> Result<WindowsPath, InputError> {
        if s.is_empty() || s.len() > WINDOWS_PATH_MAX {
            return Err(InputError::Length);
        }
        if s.starts_with(r"\\?\") || s.starts_with(r"\\.\") || s.starts_with(r"\??\") {
            return Err(InputError::Forbidden);
        }
        if !s.bytes().all(|b| b.is_ascii_graphic() || b == b' ') || s.contains('/') {
            return Err(InputError::Character);
        }
        let b = s.as_bytes();
        let folders: Vec<&str> = if let Some(unc) = s.strip_prefix(r"\\") {
            let mut parts = unc.split('\\');
            let server = parts.next().unwrap_or("");
            host_name(server)?;
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                return Err(InputError::Component);
            }
            for c in &rest {
                folder(c)?;
            }
            return Ok(WindowsPath(s.to_owned()));
        } else if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            if b.get(2) != Some(&b'\\') {
                return Err(InputError::NotAbsolute);
            }
            if b.len() == 3 {
                return Err(InputError::Forbidden);
            }
            s[3..].split('\\').collect()
        } else {
            return Err(InputError::NotAbsolute);
        };
        for c in &folders {
            folder(c)?;
        }
        if forbidden(&folders) {
            return Err(InputError::Forbidden);
        }
        Ok(WindowsPath(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether qBittorrent's `have` (which may use `/` and end in a
    /// separator) names the same folder, compared without case.
    pub fn same(&self, have: &str) -> bool {
        let norm = |s: &str| {
            let s = s.replace('/', "\\").to_ascii_lowercase();
            match s.strip_suffix('\\') {
                Some(t) if t.len() > 2 => t.to_owned(),
                _ => s,
            }
        };
        norm(have) == norm(&self.0)
    }
}

impl fmt::Display for WindowsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One folder name (see [`WindowsPath`]).
fn folder(c: &str) -> Result<(), InputError> {
    if c.is_empty() || c == "." || c == ".." {
        return Err(InputError::Component);
    }
    if !c
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b' '))
        || c.starts_with(' ')
        || c.ends_with(' ')
        || c.ends_with('.')
    {
        return Err(InputError::Character);
    }
    // Windows matches a device by the name before the first dot, with
    // trailing spaces ignored: `nul .txt` is the null device too.
    let stem = c
        .split('.')
        .next()
        .unwrap_or(c)
        .trim_end()
        .to_ascii_lowercase();
    let numbered = |p: &str| {
        stem.strip_prefix(p)
            .is_some_and(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit())
    };
    if DEVICES.contains(&stem.as_str()) || numbered("com") || numbered("lpt") {
        return Err(InputError::Reserved);
    }
    Ok(())
}

/// A UNC server: a DNS name or an IPv4 address, labels of 1–63 of
/// `A–Z a–z 0–9 -` not starting or ending with `-`.
fn host_name(s: &str) -> Result<(), InputError> {
    if s.is_empty() || s.len() > 253 {
        return Err(InputError::Component);
    }
    let ok = s.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    });
    if ok {
        Ok(())
    } else {
        Err(InputError::Character)
    }
}

/// Whether a drive path's folders (at least one) name a forbidden folder
/// or one that contains one.
fn forbidden(original: &[&str]) -> bool {
    let c: Vec<String> = original.iter().map(|x| x.to_ascii_lowercase()).collect();
    let first = c[0].as_str();
    if FORBIDDEN_ROOTS.contains(&first) || first.starts_with("program files ") {
        return true;
    }
    first == "users" && (c.len() <= 2 || c[2] == "appdata" || c[2].starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_is_the_pin_and_its_patch_releases() {
        let s = Supported::from_pin(Version::parse("5.0.3").unwrap());
        for ok in ["5.0.3", "5.0.4", "5.0.10", "5.0.3.1"] {
            assert_eq!(s.check(&Version::parse(ok).unwrap()), Ok(()), "{ok}");
        }
        for old in ["5.0.2", "4.6.7", "5.0"] {
            assert!(matches!(
                s.check(&Version::parse(old).unwrap()),
                Err(Unsupported::Older { .. })
            ));
        }
        for new in ["5.1.0", "5.1", "6.0.0", "5.2.1"] {
            assert!(matches!(
                s.check(&Version::parse(new).unwrap()),
                Err(Unsupported::Newer { .. })
            ));
        }
        let msg = s
            .check(&Version::parse("4.6.7").unwrap())
            .unwrap_err()
            .to_string();
        assert!(msg.contains("4.6.7") && msg.contains("5.0.3"), "{msg}");
    }

    #[test]
    fn steps_are_in_order_with_unique_codes() {
        let codes: Vec<_> = Step::ALL.iter().map(|s| s.code()).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len());
        assert_eq!(codes.first(), Some(&"download"));
        assert_eq!(codes.last(), Some(&"connect"));
    }

    #[test]
    fn device_names_are_refused_in_any_spelling() {
        for d in [
            "CON",
            "con",
            "Nul",
            "nul.txt",
            "NUL .txt",
            "aux.tar.gz",
            "COM1",
            "lpt9",
            "COM0",
            "prn",
        ] {
            assert_eq!(folder(d), Err(InputError::Reserved), "{d}");
        }
        for ok in [
            "console",
            "com10",
            "nullable",
            "lpt",
            "auxiliary",
            "CONx.txt",
        ] {
            assert_eq!(folder(ok), Ok(()), "{ok}");
        }
    }
}
