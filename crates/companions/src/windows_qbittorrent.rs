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
//! refuses one outside the range ([`Supported`]) with
//! [`VERSION_UNSUPPORTED`] before anything is written. The range is the
//! pin's once the installer is pinned, and until then the releases CMP-8b
//! was checked against ([`CHECKED_RANGE`]).
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
use std::sync::{Arc, Mutex};

use crate::adapter::{
    AdapterError, BoxFuture, CheckedPlan, ConfigPlan, Configure, ConfigureInput, GuardedHttp,
    Target,
};
use crate::catalogue::{Arch, ArchiveKind, Entry, NotAvailable, Pin, Platform};
use crate::configure::{self, qbittorrent};
use crate::inputs::{CompanionId, InputError, Version};

/// qBittorrent's own download page. The installer is linked from there;
/// Lodestar shows this and never fetches it.
pub const DOWNLOAD_PAGE: &str = "https://www.qbittorrent.org/download";

/// The refusal for a qBittorrent outside the pinned version's range.
pub const VERSION_UNSUPPORTED: &str = "qbittorrent_version_unsupported";
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

/// The qBittorrent releases CMP-8b's adapter was checked against, which
/// the gate accepts while the Windows installer awaits its pin: from
/// `4.6.0` up to, not including, `5.3`. CMP-8b checked `release-4.6.3`,
/// `5.0.3`, `5.1.2` and `5.2.0` and supports every line from 4.6
/// (`docs/decisions/2026-11-08-CMP-8b.md`, "qBittorrent (WebUI API v2)").
/// Once the installer is pinned, the pin's range replaces it (runner,
/// 2026-11-11, CMP-11b option (D)).
pub const CHECKED_RANGE: (&str, (u32, u32)) = ("4.6.0", (5, 3));

/// The range [`CHECKED_RANGE`] names, for messages: `4.6.0 to 5.2.x`.
fn checked_text() -> String {
    let (min, (ma, mi)) = CHECKED_RANGE;
    match mi.checked_sub(1) {
        Some(prev) => format!("{min} to {ma}.{prev}.x"),
        None => format!("{min} up to {ma}.{mi}"),
    }
}

/// The versions the gate accepts: at least `min`, and a `major.minor`
/// below `below`.
/// - **Pinned:** the pinned version and later patch releases of the same
///   `major.minor` (`5.0.3` accepts `5.0.3` to `5.0.x`, not `5.0.2` or
///   `5.1.0`). An older one may lack the pin's fixes; a newer minor
///   release is one Lodestar hasn't checked CMP-8b's adapter against.
/// - **Unpinned:** [`CHECKED_RANGE`].
///
/// Neither contains the other: a pin at `5.0.3` refuses `4.6.7`, which
/// the checked range accepts, and a later pin at `5.3.1` would accept
/// `5.3.2`, which it refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supported {
    min: Version,
    below: (u32, u32),
    pinned: bool,
}

/// Why a version is outside [`Supported`]. Carries only Lodestar's own
/// parsed versions, so its message can be shown as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// Older than `min`: the pinned version, or the oldest checked one.
    Older {
        found: Version,
        min: Version,
        pinned: bool,
    },
    /// A newer `major.minor` than Lodestar accepts. `pinned` is the pinned
    /// version, or `None` under [`CHECKED_RANGE`].
    Newer {
        found: Version,
        pinned: Option<Version>,
    },
}

/// A version's `major.minor`; `None` for a part that doesn't read
/// (`Version` holds digits only, so that never happens), which then counts
/// as outside every range rather than as `0`.
fn major_minor(v: &Version) -> Option<(u32, u32)> {
    let mut it = v.as_str().split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next().unwrap_or(Some(0))?))
}

impl Supported {
    /// The pin's range.
    pub fn from_pin(pinned: Version) -> Supported {
        // `major.minor + 1`; an unreadable or overflowing pin accepts
        // nothing (`below` of `(0, 0)`).
        let below = major_minor(&pinned)
            .and_then(|(ma, mi)| Some((ma, mi.checked_add(1)?)))
            .unwrap_or((0, 0));
        Supported {
            min: pinned,
            below,
            pinned: true,
        }
    }

    /// [`CHECKED_RANGE`], while the installer awaits its pin.
    pub fn checked() -> Supported {
        let (min, below) = CHECKED_RANGE;
        Supported {
            min: Version::parse(min).expect("a valid constant"),
            below,
            pinned: false,
        }
    }

    pub fn check(&self, found: &Version) -> Result<(), Unsupported> {
        if *found < self.min {
            return Err(Unsupported::Older {
                found: found.clone(),
                min: self.min.clone(),
                pinned: self.pinned,
            });
        }
        match major_minor(found) {
            Some(mm) if mm < self.below => Ok(()),
            _ => Err(Unsupported::Newer {
                found: found.clone(),
                pinned: self.pinned.then(|| self.min.clone()),
            }),
        }
    }
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unsupported::Older {
                found,
                min,
                pinned: true,
            } => write!(
                f,
                "This qBittorrent is {found}, older than {min}, the version Lodestar \
                 supports on Windows. Install {min} from qBittorrent's download page, then \
                 try again."
            ),
            Unsupported::Older { found, .. } => write!(
                f,
                "This qBittorrent is {found}. On Windows, this release of Lodestar supports \
                 qBittorrent {range}. Install a release in that range from qBittorrent's \
                 official site, then try again.",
                range = checked_text()
            ),
            Unsupported::Newer {
                found,
                pinned: Some(pinned),
            } => write!(
                f,
                "This qBittorrent is {found}, newer than the {pinned} release Lodestar has \
                 checked on Windows. Install {pinned} from qBittorrent's download page, or \
                 wait for a Lodestar update that supports {found}."
            ),
            Unsupported::Newer { found, .. } => write!(
                f,
                "This qBittorrent is {found}, newer than this release of Lodestar has checked \
                 on Windows: it supports qBittorrent {range}. Wait for a Lodestar update that \
                 supports {found}, or install a release in that range from qBittorrent's \
                 official site.",
                range = checked_text()
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
                    "This release of Lodestar has no checked qBittorrent installer yet, so \
                     there's no checksum to compare. Open {page}, qBittorrent's official \
                     download page, and download its 64-bit Windows installer for a release \
                     from {range}: Lodestar can configure only those. If the newest release \
                     there is later than that, wait for a Lodestar update. Lodestar doesn't \
                     download or run it for you.",
                    range = checked_text()
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

/// CMP-8b's qBittorrent adapter with the Windows version gate. Signed in,
/// on the adapter's own session (so it costs no sign-in of its own), a
/// version outside the pin's range is refused before anything is read or
/// written, in the plan and again in the apply. [`configurator`] is what
/// `configure::configurator` hands out for qBittorrent on Windows
/// (CMP-11b).
pub struct WindowsQBittorrent {
    id: CompanionId,
    /// `None` only if the compiled-in catalogue couldn't be read: then
    /// every call refuses ([`NO_INSTALLER`]).
    entry: Option<&'static Entry>,
    inner: qbittorrent::QBittorrent,
    refused: Arc<Mutex<Option<Unsupported>>>,
}

impl fmt::Debug for WindowsQBittorrent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsQBittorrent")
            .field("companion", &self.id)
            .finish_non_exhaustive()
    }
}

impl WindowsQBittorrent {
    /// For `entry`, the catalogue's qBittorrent.
    pub fn new(entry: &'static Entry) -> WindowsQBittorrent {
        WindowsQBittorrent::with(entry.id.clone(), Some(entry))
    }

    fn with(id: CompanionId, entry: Option<&'static Entry>) -> WindowsQBittorrent {
        let refused = Arc::new(Mutex::new(None));
        let supported = entry.and_then(supported_for);
        let seen = Arc::clone(&refused);
        let gate: qbittorrent::VersionGate = Box::new(move |found: &Version| {
            // No installer never reaches here (`ready`); refuse all the same.
            let Some(s) = &supported else {
                return Err(AdapterError::Refused(NO_INSTALLER));
            };
            let r = s.check(found);
            let ok = r.is_ok();
            if let Ok(mut last) = seen.lock() {
                *last = r.err();
            }
            if ok {
                Ok(())
            } else {
                Err(AdapterError::Refused(VERSION_UNSUPPORTED))
            }
        });
        WindowsQBittorrent {
            inner: qbittorrent::QBittorrent::gated(id.clone(), gate),
            id,
            entry,
            refused,
        }
    }

    /// Refuses before anything is sent when there's no Windows installer
    /// to check against: no catalogue, or no installer in it.
    fn ready(&self) -> Result<(), AdapterError> {
        self.entry
            .and_then(supported_for)
            .map(|_| ())
            .ok_or(AdapterError::Refused(NO_INSTALLER))
    }

    /// Why the last call was refused with [`VERSION_UNSUPPORTED`]: the
    /// version found and the pinned one, for the screen's plain message
    /// (its `Display`). `None` after a call that wasn't.
    ///
    /// One slot shared by every call on this configurator, cleared as each
    /// starts: two plans run at once can overwrite each other's. The guided
    /// path runs one at a time.
    pub fn last_refusal(&self) -> Option<Unsupported> {
        self.refused.lock().ok()?.clone()
    }

    /// Why `found` is refused, as a plain message, or `None` when it's
    /// supported. For the screen's text beside [`VERSION_UNSUPPORTED`].
    pub fn explain(&self, found: &Version) -> Option<String> {
        let s = supported_for(self.entry?)?;
        s.check(found).err().map(|u| u.to_string())
    }

    fn clear(&self) {
        if let Ok(mut last) = self.refused.lock() {
            *last = None;
        }
    }
}

impl Configure for WindowsQBittorrent {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn plan<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
    ) -> BoxFuture<'a, Result<ConfigPlan, AdapterError>> {
        Box::pin(async move {
            self.clear();
            self.ready()
                .map_err(|e| configure::failed(qbittorrent::ID, "version", e))?;
            self.inner.plan(http, target, input).await
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
            self.clear();
            self.ready()
                .map_err(|e| configure::failed(qbittorrent::ID, "version", e))?;
            // An empty plan writes nothing, so CMP-8b sends nothing and
            // there's no session to check on.
            self.inner.apply_plan(http, target, input, plan).await
        })
    }
}

/// The range for `entry`'s Windows installer: the pin's once pinned,
/// [`CHECKED_RANGE`] while it awaits its pin, and `None` without one.
pub fn supported_for(entry: &Entry) -> Option<Supported> {
    match Installer::of(entry) {
        Ok(i) => Some(i.supported()),
        Err(NotAvailable::AwaitingPin) => Some(Supported::checked()),
        Err(_) => None,
    }
}

/// The Windows configurator for qBittorrent, from the compiled-in
/// catalogue. Always one: without the catalogue it refuses every call.
/// `configure::configurator` hands it out under `cfg(windows)` (CMP-11b).
pub fn configurator(id: &CompanionId) -> Option<Box<dyn Configure>> {
    if id.as_str() != qbittorrent::ID {
        return None;
    }
    let entry = crate::builtin().ok().and_then(|c| c.get(id.as_str()));
    Some(Box::new(WindowsQBittorrent::with(id.clone(), entry)))
}

// ─── Windows save paths ─────────────────────────────────────────────────

/// The longest Windows path accepted, in bytes: `CreateDirectoryW`'s limit
/// without `\\?\` is `MAX_PATH - 12` (248) including the terminating null.
pub const WINDOWS_PATH_MAX: usize = 247;

/// Device names Windows opens in any folder and with any extension
/// (`C:\x\nul.txt` is the null device).
const DEVICES: &[&str] = &["con", "prn", "aux", "nul"];

/// Folders directly in a drive's root (or a share's) that a companion is
/// never given, with everything below them. `documents and settings` is a
/// junction to `Users`.
const FORBIDDEN_ROOTS: &[&str] = &[
    "windows",
    "windows.old",
    "program files",
    "programdata",
    "system volume information",
    "recovery",
    "boot",
    "perflogs",
    "documents and settings",
];

/// Junctions in `Users` that Windows ships: `All Users` is
/// `C:\ProgramData` (Lodestar's own data), `Default User` is `Default`.
const USERS_JUNCTIONS: &[&str] = &["all users", "default user"];

/// Folders directly in a profile that a companion is never given:
/// `AppData`, and the junctions Windows ships into it.
const PROFILE_FORBIDDEN: &[&str] = &[
    "appdata",
    "application data",
    "local settings",
    "cookies",
    "nethood",
    "printhood",
    "recent",
    "sendto",
    "start menu",
    "templates",
];

/// Share names that aren't folders: the named-pipe and mailslot
/// namespaces reached over UNC.
const RESERVED_SHARES: &[&str] = &["pipe", "mailslot"];

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
/// - the junctions Windows ships into those: `Documents and Settings`,
///   `Users\All Users` (`ProgramData`), `Users\Default User`, and a
///   profile's `Application Data`, `Local Settings`, `Cookies`, `NetHood`,
///   `PrintHood`, `Recent`, `SendTo`, `Start Menu` and `Templates`;
/// - for a UNC path, a loopback server (`localhost`, `127.x`, all-numeric
///   short forms), the `pipe` and `mailslot` shares, and the same folders
///   as above after the share (a share may be a drive's root).
///
/// Compared without case, as Windows does. Lodestar's own data on Windows
/// is under `C:\ProgramData\Lodestar`, which the list keeps out.
///
/// **What this can't see:** it checks the path as typed. These pass:
/// - a junction or link you made yourself;
/// - this machine reached by its own name or a LAN address;
/// - a drive's root shared under another name than its folder's
///   (`\\nas\C\Windows`): the folders after a share are checked only as
///   if the share were named after the root folder.
///
/// `X:\Windows.old` (an earlier install and its profiles) is refused with
/// `Windows`. `X:\Users\Default\…`, the template profile, passes like
/// any other profile's folders: below it, `AppData` and dot folders are
/// still refused.
///
/// The guide says to pick a plain downloads folder.
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
            if loopback(server) {
                return Err(InputError::Forbidden);
            }
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                return Err(InputError::Component);
            }
            for c in &rest {
                folder(c)?;
            }
            if RESERVED_SHARES.contains(&rest[0].to_ascii_lowercase().as_str()) {
                return Err(InputError::Forbidden);
            }
            rest
        } else if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            if b.get(2) != Some(&b'\\') {
                return Err(InputError::NotAbsolute);
            }
            if b.len() == 3 {
                return Err(InputError::Forbidden);
            }
            let f: Vec<&str> = s[3..].split('\\').collect();
            for c in &f {
                folder(c)?;
            }
            f
        } else {
            return Err(InputError::NotAbsolute);
        };
        // A share may be a drive's root shared (`\\nas\Users`), so the same
        // folders are refused after it.
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

/// Whether a UNC server may be this machine by a loopback name or address:
/// - `localhost`, names below it, and names ending in `.ipv6-literal.net`
///   (Windows' name form for an IPv6 address);
/// - any label in octal (`0177`, a digit string with a leading `0`) or hex
///   (`0x7f`), which Windows' address parser reads in that base;
/// - any all-numeric name, which Windows may read as an IPv4 address in a
///   short or decimal form (`127.1`, `2130706433`), unless it's a full
///   dotted address outside `127.0.0.0/8` and `0.0.0.0/8`.
///
/// The machine's own name can't be known here (see [`WindowsPath`]).
fn loopback(server: &str) -> bool {
    let s = server.to_ascii_lowercase();
    if s == "localhost" || s.ends_with(".localhost") || s.ends_with(".ipv6-literal.net") {
        return true;
    }
    // Windows' address parser reads `0177` as octal and `0x7f` as hex, so
    // any such label may be an address in another spelling (#705's review,
    // I1): refused rather than decoded.
    let other_base = |l: &str| {
        (l.len() > 1 && l.starts_with('0') && l.bytes().all(|b| b.is_ascii_digit()))
            || (l.starts_with("0x") && l[2..].bytes().all(|b| b.is_ascii_hexdigit()))
    };
    if s.split('.').any(other_base) {
        return true;
    }
    if !s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return false;
    }
    let octets: Vec<Option<u8>> = s.split('.').map(|o| o.parse().ok()).collect();
    !(octets.len() == 4
        && octets.iter().all(Option::is_some)
        && !matches!(octets[0], Some(0 | 127)))
}

/// Whether a path's folders (at least one; after the drive, or the share
/// and what follows it) name a forbidden folder or one that contains one.
fn forbidden(original: &[&str]) -> bool {
    let c: Vec<String> = original.iter().map(|x| x.to_ascii_lowercase()).collect();
    let first = c[0].as_str();
    if FORBIDDEN_ROOTS.contains(&first) || first.starts_with("program files ") {
        return true;
    }
    first == "users"
        && (c.len() <= 2
            || USERS_JUNCTIONS.contains(&c[1].as_str())
            || PROFILE_FORBIDDEN.contains(&c[2].as_str())
            || c[2].starts_with('.'))
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

    /// While unpinned: exactly CMP-8b's checked range.
    #[test]
    fn the_checked_range_is_cmp_8bs() {
        let s = Supported::checked();
        for ok in [
            "4.6.0", "4.6.3", "4.7.1", "5.0.3", "5.1.2", "5.2.0", "5.2.9",
        ] {
            assert_eq!(s.check(&Version::parse(ok).unwrap()), Ok(()), "{ok}");
        }
        for old in ["4.5.9", "4.5", "3.3.16"] {
            assert!(matches!(
                s.check(&Version::parse(old).unwrap()),
                Err(Unsupported::Older { pinned: false, .. })
            ));
        }
        for new in ["5.3", "5.3.0", "5.10.1", "6.0.0"] {
            assert!(matches!(
                s.check(&Version::parse(new).unwrap()),
                Err(Unsupported::Newer { pinned: None, .. })
            ));
        }
        let msg = s
            .check(&Version::parse("5.3.0").unwrap())
            .unwrap_err()
            .to_string();
        assert!(msg.contains("5.3.0") && msg.contains("5.2"), "{msg}");
    }

    /// A pin's range replaces the checked one, and neither contains the
    /// other.
    #[test]
    fn a_pin_replaces_the_checked_range() {
        let pin = Supported::from_pin(Version::parse("5.0.3").unwrap());
        let later = Supported::from_pin(Version::parse("5.3.1").unwrap());
        let checked = Supported::checked();
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(checked.check(&v("4.6.7")).is_ok() && pin.check(&v("4.6.7")).is_err());
        assert!(checked.check(&v("5.3.2")).is_err() && later.check(&v("5.3.2")).is_ok());
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
