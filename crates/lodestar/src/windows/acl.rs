//! Making sure the service's default data folder is private (architecture
//! §11, §12). `%ProgramData%` lets every local user read and create
//! folders and files, and a new folder inherits that. Lodestar keeps its
//! session key (`auth.json`), its log (with the setup token) and its
//! database there, and the service reads `config.toml` from it, so before
//! anything is read or written the folder must be private:
//! - its owner is `SYSTEM`, `Administrators` or the service's own SID
//!   (`NT SERVICE\<name>`, which owns a folder it created): an owner can
//!   always rewrite the ACL;
//! - its DACL is protected (nothing inherited) and allows only those two
//!   and the service's own SID (`NT SERVICE\<name>`). A shared account
//!   such as LocalService isn't enough: many other services run as it;
//! - nothing directly inside it is owned by anyone else, and nothing
//!   anywhere inside it is a link, junction or other reparse point: such an
//!   entry could have been planted before the folder was private.
//!
//! These are the installer's rules too (`packaging/windows/
//! secure-datadir.ps1`, `docs/decisions/2026-10-22-P-release.md`); the two
//! must stay in step. A `.lodestar-secured` marker is written once the
//! folder passes, as a hint for the installer only: nothing trusts it.
//!
//! This is checked at every start. A missing folder is created with its
//! final ACL in one step. The service never repairs a folder: one that
//! isn't private is refused, and only the installer secures an existing
//! folder. (A user who created the folder can keep a handle that survives
//! any owner or ACL change made afterwards.)
//!
//! The policy and the SDDL parsing are plain code tested on every OS;
//! [`Windows`] is the system side. The workspace forbids `unsafe`, so it
//! reads ACLs with PowerShell's `Get-Acl` (SDDL isn't localised) and
//! creates the folder through .NET, both by absolute path.

use std::io;
use std::path::Path;

/// Written once the folder passes; a hint for the installer, trusted by
/// nothing.
pub const MARKER: &str = ".lodestar-secured";

/// One security descriptor in SDDL, as far as the check needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// `O:`, as SDDL writes it (`BA`, `SY`, `S-1-5-80-…`).
    pub owner: String,
    /// `D:`, as SDDL writes it.
    pub dacl: String,
}

/// Splits SDDL (`O:BAG:SYD:PAI(A;OICI;FA;;;SY)…`) into owner and DACL.
pub fn parse_sddl(sddl: &str) -> Result<Descriptor, String> {
    let sddl = sddl.trim();
    let rest = sddl
        .strip_prefix("O:")
        .ok_or("no owner in its security descriptor")?;
    // SIDs and aliases never contain ':', so the owner ends where the next
    // component's letter-and-colon starts.
    let end = rest
        .find(':')
        .map(|i| i.saturating_sub(1))
        .unwrap_or(rest.len());
    let owner = rest[..end].to_string();
    let after = &rest[end..];
    let dacl = after
        .strip_prefix("D:")
        .or_else(|| {
            // `G:<group>` sits between the owner and the DACL.
            let g = after.strip_prefix("G:")?;
            let i = g.find("D:")?;
            Some(&g[i + 2..])
        })
        .ok_or("no DACL in its security descriptor")?;
    if owner.is_empty() {
        return Err("no owner in its security descriptor".into());
    }
    Ok(Descriptor {
        owner,
        dacl: dacl.to_string(),
    })
}

/// `SYSTEM`, `Administrators` or the service's own SID.
pub fn trusted(sid: &str, own_sid: Option<&str>) -> bool {
    matches!(sid, "SY" | "BA" | "S-1-5-18" | "S-1-5-32-544") || Some(sid) == own_sid
}

/// Checks a DACL in SDDL (the part after `D:`), strictly: flags only `P`
/// (required), `AI` and `AR`; entries only allow (`A`) for a [trusted]
/// SID and deny (`D`); nothing after the last entry but a SACL. A NULL
/// DACL (`NO_ACCESS_CONTROL`), object or conditional entries, and
/// anything unparsed are refused.
pub fn check_dacl(dacl: &str, own_sid: Option<&str>) -> Result<(), String> {
    let mut rest = dacl;
    let mut protected = false;
    while !rest.is_empty() && !rest.starts_with('(') && !rest.starts_with("S:") {
        if rest.starts_with("NO_ACCESS_CONTROL") {
            return Err("it has no access control at all (a NULL DACL)".into());
        } else if let Some(r) = rest.strip_prefix("AI").or_else(|| rest.strip_prefix("AR")) {
            rest = r;
        } else if let Some(r) = rest.strip_prefix('P') {
            protected = true;
            rest = r;
        } else {
            return Err("its permissions have flags Lodestar doesn't know".into());
        }
    }
    if !protected {
        return Err("it inherits permissions from its parent folder".into());
    }
    while let Some(body) = rest.strip_prefix('(') {
        let end = body.find(')').ok_or("a malformed entry")?;
        let fields: Vec<&str> = body[..end].split(';').collect();
        let [kind, _, _, _, _, sid] = fields[..] else {
            return Err("an entry Lodestar doesn't know".into());
        };
        match kind {
            "A" if trusted(sid, own_sid) => {}
            "A" => return Err(format!("it grants access to {sid}")),
            "D" => {}
            _ => return Err(format!("it has an entry of type {kind}")),
        }
        rest = &body[end + 1..];
    }
    if rest.is_empty() || rest.starts_with("S:") {
        Ok(())
    } else {
        Err("a malformed entry".into())
    }
}

/// What [`Ops::inspect`] reports: the folder and each entry directly in
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub folder: Item,
    pub entries: Vec<Item>,
    /// Reparse points anywhere beneath the folder, by full path.
    pub links: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    /// A reparse point: a symbolic link, junction or mount point.
    pub link: bool,
    /// Its security descriptor; empty for a link (not read).
    pub sddl: String,
}

/// Parses the inspection script's output: one tab-separated line per item,
/// `F` for the folder, `E` per entry directly in it, `L` per reparse point
/// anywhere beneath it, each `kind, attributes, SDDL, name`.
pub fn parse_inspection(text: &str) -> Result<Inspection, String> {
    let mut folder = None;
    let mut entries = Vec::new();
    let mut links = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut f = line.splitn(4, '\t');
        let (Some(kind), Some(attrs), Some(sddl), Some(name)) =
            (f.next(), f.next(), f.next(), f.next())
        else {
            return Err(format!("unexpected inspection line {line:?}"));
        };
        let item = Item {
            name: name.trim_end().to_string(),
            link: attrs.contains("ReparsePoint"),
            sddl: sddl.to_string(),
        };
        match kind {
            "F" if folder.is_none() => folder = Some(item),
            "E" => entries.push(item),
            "L" => links.push(item.name),
            _ => return Err(format!("unexpected inspection line {line:?}")),
        }
    }
    Ok(Inspection {
        folder: folder.ok_or("the inspection didn't report the folder")?,
        entries,
        links,
    })
}

/// Why an inspected folder isn't private, or `Ok` when it is.
pub fn verdict(seen: &Inspection, own_sid: Option<&str>) -> Result<(), String> {
    if seen.folder.link {
        return Err("it's a link or junction; Lodestar only uses a real folder".into());
    }
    if let Some(link) = seen.links.first() {
        return Err(format!("{link} is a link or junction inside it"));
    }
    for e in &seen.entries {
        if e.link {
            return Err(format!("{} in it is a link or junction", e.name));
        }
        let d = parse_sddl(&e.sddl).map_err(|why| format!("{}: {why}", e.name))?;
        if !trusted(&d.owner, own_sid) {
            return Err(format!(
                "{} in it belongs to {}, so another user could have put it there",
                e.name, d.owner
            ));
        }
    }
    let d = parse_sddl(&seen.folder.sddl)?;
    if !trusted(&d.owner, own_sid) {
        return Err(format!("it belongs to {}", d.owner));
    }
    check_dacl(&d.dacl, own_sid)
}

/// The system side of [`secure_dir`].
pub trait Ops {
    /// `None` when the folder doesn't exist.
    fn inspect(&self, dir: &Path) -> io::Result<Option<Inspection>>;
    /// Creates the folder with its final ACL in one step.
    fn create(&self, dir: &Path) -> io::Result<()>;
}

/// What a refusal tells the administrator to do.
pub const REPAIR: &str = "Repair the installation: run the Lodestar installer's Repair, or \
                          delete the folder as an administrator and start the service again";

/// Makes sure `dir` is private for the service with `own_sid`, creating
/// it if needed. It never changes an existing folder. An error says why
/// the service won't use it.
pub fn secure_dir(dir: &Path, own_sid: Option<&str>, ops: &impl Ops) -> Result<(), String> {
    let shown = dir.display();
    let inspect = || {
        ops.inspect(dir)
            .map_err(|e| format!("reading {shown}'s permissions: {e}"))
    };
    let seen = match inspect()? {
        Some(seen) => seen,
        None => {
            ops.create(dir)
                .map_err(|e| format!("creating {shown}: {e}"))?;
            inspect()?.ok_or_else(|| format!("{shown} vanished after it was created"))?
        }
    };
    verdict(&seen, own_sid).map_err(|why| format!("{shown} isn't private: {why}. {REPAIR}."))?;
    let marker = dir.join(MARKER);
    if !marker.exists() {
        let _ = std::fs::write(marker, "Lodestar checked this folder's permissions.\r\n");
    }
    Ok(())
}

/// The private DACL, for creating the folder.
pub fn private_sddl(own_sid: Option<&str>) -> String {
    let mut s = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)".to_string();
    if let Some(sid) = own_sid {
        s.push_str(&format!("(A;OICI;FA;;;{sid})"));
    }
    s
}

/// The service SID in `sc showsid` output (its labels are translated; the
/// SID isn't).
pub fn parse_service_sid(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|w| {
            w.strip_prefix("S-1-5-80-")
                .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit() || c == '-'))
        })
        .map(String::from)
}

/// Lists the folder and what's directly in it for [`parse_inspection`].
/// The path arrives in an environment variable, so it's never parsed as
/// script. Only cmdlets and property reads, which Constrained Language
/// Mode allows.
pub const INSPECT_SCRIPT: &str = "$ErrorActionPreference = 'Stop'
$p = $env:LODESTAR_ACL_PATH
$f = Get-Item -LiteralPath $p -Force
$s = if (\"$($f.Attributes)\" -match 'ReparsePoint') { '' } else { (Get-Acl -LiteralPath $p).Sddl }
\"F`t$($f.Attributes)`t$s`t$($f.Name)\"
if (\"$($f.Attributes)\" -match 'ReparsePoint') { exit 0 }
foreach ($c in Get-ChildItem -LiteralPath $p -Force) {
  $s = if (\"$($c.Attributes)\" -match 'ReparsePoint') { '' } else { (Get-Acl -LiteralPath $c.FullName).Sddl }
  \"E`t$($c.Attributes)`t$s`t$($c.Name)\"
}
foreach ($l in Get-ChildItem -LiteralPath $p -Force -Recurse -Attributes ReparsePoint) {
  \"L`t$($l.Attributes)`t`t$($l.FullName)\"
}";

/// Creates the folder with its ACL in one call (no window with the
/// inherited one). Needs Full Language Mode. On a path that exists by
/// then (a race), it changes nothing, and the inspection that follows
/// judges what's there.
pub const CREATE_SCRIPT: &str = "$ErrorActionPreference = 'Stop'
$s = New-Object System.Security.AccessControl.DirectorySecurity
$s.SetSecurityDescriptorSddlForm($env:LODESTAR_ACL_SDDL)
$null = [System.IO.Directory]::CreateDirectory($env:LODESTAR_ACL_PATH, $s)";

/// [`Ops`] on Windows, for the service with `own_sid`.
#[cfg(windows)]
#[derive(Debug)]
pub struct Windows<'a> {
    pub own_sid: Option<&'a str>,
}

#[cfg(windows)]
impl Windows<'_> {
    fn powershell(&self, script: &str, dir: &Path) -> io::Result<String> {
        let mut cmd =
            super::powershell_command().ok_or_else(|| io::Error::other("SystemRoot isn't set"))?;
        let out = cmd
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ])
            .env("LODESTAR_ACL_PATH", dir)
            .env("LODESTAR_ACL_SDDL", private_sddl(self.own_sid))
            .output()?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ))
        }
    }
}

#[cfg(windows)]
impl Ops for Windows<'_> {
    fn inspect(&self, dir: &Path) -> io::Result<Option<Inspection>> {
        match std::fs::symlink_metadata(dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(_) => {}
        }
        let text = self.powershell(INSPECT_SCRIPT, dir)?;
        parse_inspection(&text).map(Some).map_err(io::Error::other)
    }

    fn create(&self, dir: &Path) -> io::Result<()> {
        // Under Constrained Language Mode this call isn't allowed; the
        // service then refuses, and the installer has to create the folder.
        self.powershell(CREATE_SCRIPT, dir).map(drop)
    }
}

/// The SID of service `name` (`NT SERVICE\<name>`), from `sc showsid`.
#[cfg(windows)]
pub fn service_sid(name: &str) -> io::Result<String> {
    let out = super::system32_command("sc.exe")
        .ok_or_else(|| io::Error::other("SystemRoot isn't set"))?
        .args(["showsid", name])
        .output()?;
    parse_service_sid(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| io::Error::other(format!("no service SID for {name}")))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// The service's own SID (`NT SERVICE\Lodestar`).
    const OWN: &str = "S-1-5-80-1-2-3-4-5";
    const PRIVATE: &str =
        "O:BAG:SYD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;S-1-5-80-1-2-3-4-5)";
    const INHERITED: &str = "O:S-1-5-21-9-9-9-1001G:S-1-5-21-9-9-9-513D:AI(A;OICIID;FA;;;SY)\
                             (A;OICIID;FA;;;BA)(A;OICIID;0x1200a9;;;BU)";

    fn check(dacl: &str) -> Result<(), String> {
        check_dacl(dacl, Some(OWN))
    }

    #[test]
    fn splits_owner_and_dacl() {
        let d = parse_sddl(PRIVATE).unwrap();
        assert_eq!(d.owner, "BA");
        assert!(d.dacl.starts_with("PAI(A;OICI;FA;;;SY)"));
        let d = parse_sddl("O:S-1-5-80-1-2-3-4-5D:P(A;;FA;;;SY)").unwrap();
        assert_eq!(d.owner, OWN);
        assert_eq!(d.dacl, "P(A;;FA;;;SY)");
        assert_eq!(parse_sddl(INHERITED).unwrap().owner, "S-1-5-21-9-9-9-1001");
        assert!(parse_sddl("D:P(A;;FA;;;SY)").is_err());
        assert!(parse_sddl("O:BAG:SY").is_err());
        assert!(parse_sddl("").is_err());
    }

    #[test]
    fn checks_the_dacl_strictly() {
        assert_eq!(
            check("PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;S-1-5-80-1-2-3-4-5)"),
            Ok(())
        );
        assert_eq!(check("PARAI(A;;FA;;;S-1-5-18)(D;;FA;;;BU)"), Ok(()));
        assert_eq!(check("P(A;;FA;;;SY)S:(AU;SAFA;FA;;;WD)"), Ok(()));
        assert_eq!(check("P"), Ok(()));
        assert!(
            check("AI(A;OICIID;FA;;;SY)")
                .unwrap_err()
                .contains("inherits")
        );
        // A NULL DACL contains a P but allows everyone.
        assert!(check("PNO_ACCESS_CONTROL").unwrap_err().contains("NULL"));
        assert!(check("NO_ACCESS_CONTROL").is_err());
        assert!(check("PX(A;;FA;;;SY)").is_err());
        // Object and conditional allow entries are refused, whatever SID.
        assert!(check("P(OA;;FA;;;SY)").is_err());
        assert!(check("P(OA;;FA;bf967aba-0de6-11d0-a285-00aa003049e2;;BU)").is_err());
        assert!(check("P(XA;;FA;;;SY;(x))").is_err());
        assert!(check("P(AU;;FA;;;SY)").is_err());
        // Shared service accounts and other services' SIDs aren't enough.
        for sid in [
            "BU",
            "WD",
            "AU",
            "LS",
            "NS",
            "S-1-5-19",
            "S-1-5-80-9-9-9-9-9",
            "S-1-5-21-1-2-3-1001",
        ] {
            assert!(
                check(&format!("P(A;;FA;;;SY)(A;;FA;;;{sid})")).is_err(),
                "{sid}"
            );
        }
        // Without its own SID, the service's entry isn't trusted.
        assert!(check_dacl("P(A;;FA;;;S-1-5-80-1-2-3-4-5)", None).is_err());
        // Malformed.
        assert!(check("P(A;;FA;;SY)").is_err());
        assert!(check("P(A;;FA;;;SY").is_err());
        assert!(check("P(A;;FA;;;SY)junk").is_err());
    }

    fn item(name: &str, sddl: &str) -> Item {
        Item {
            name: name.into(),
            link: false,
            sddl: sddl.into(),
        }
    }

    fn seen(folder: &str, entries: &[Item]) -> Inspection {
        Inspection {
            folder: item("Lodestar", folder),
            entries: entries.to_vec(),
            links: Vec::new(),
        }
    }

    #[test]
    fn judges_the_folder_and_what_is_in_it() {
        let own_file = item("auth.json", "O:S-1-5-80-1-2-3-4-5D:AI(A;ID;FA;;;SY)");
        let admin_file = item("config.toml", "O:BAD:AI(A;ID;FA;;;SY)");
        let user_file = item("config.toml", "O:S-1-5-21-9-9-9-1001D:AI(A;ID;FA;;;SY)");
        let v = |s: &Inspection| verdict(s, Some(OWN));
        let refused = |s: &Inspection, why: &str| {
            let err = v(s).unwrap_err();
            assert!(err.contains(why), "{err}");
        };
        assert_eq!(v(&seen(PRIVATE, &[])), Ok(()));
        assert_eq!(v(&seen(PRIVATE, &[own_file.clone(), admin_file])), Ok(()));
        // The service owns a folder it created itself.
        let service_owned =
            format!("O:{OWN}D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{OWN})");
        assert_eq!(v(&seen(&service_owned, &[])), Ok(()));
        // A planted entry is refused, even in a private folder.
        refused(
            &seen(PRIVATE, std::slice::from_ref(&user_file)),
            "belongs to S-1-5-21",
        );
        let link = Item {
            link: true,
            sddl: String::new(),
            ..item("logs", "")
        };
        refused(&seen(PRIVATE, &[link]), "logs in it is a link");
        // A folder that isn't private is refused, empty or not: the
        // service never repairs one.
        refused(&seen(INHERITED, &[]), "belongs to S-1-5-21");
        let admin_open = "O:BAD:AI(A;OICIID;FA;;;SY)(A;OICIID;0x1200a9;;;BU)";
        refused(&seen(admin_open, &[]), "inherits");
        refused(&seen(admin_open, &[own_file]), "inherits");
        // A user-owned folder with a private-looking DACL isn't private.
        refused(
            &seen(
                "O:S-1-5-21-9-9-9-1001D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)",
                &[],
            ),
            "belongs to S-1-5-21",
        );
        // Another service's SID owns nothing here.
        refused(
            &seen("O:S-1-5-80-9-9-9-9-9D:PAI(A;OICI;FA;;;SY)", &[]),
            "belongs to S-1-5-80-9",
        );
        // A link anywhere beneath it is refused.
        let mut deep = seen(PRIVATE, &[]);
        deep.links.push(r"C:\ProgramData\Lodestar\index\v4".into());
        refused(&deep, r"index\v4 is a link");
        // A linked folder is refused outright.
        let mut linked = seen(PRIVATE, &[]);
        linked.folder.link = true;
        refused(&linked, "link or junction");
    }

    #[test]
    fn parses_the_inspection() {
        let text = format!(
            "F\tDirectory\t{PRIVATE}\tLodestar\r\n\
             E\tArchive\tO:BAD:AI(A;ID;FA;;;SY)\tconfig.toml\r\n\
             E\tDirectory, ReparsePoint\t\tlogs\r\n\
             E\tArchive\tO:SYD:AI\tname\twith tab\r\n\
             L\tDirectory, ReparsePoint\t\tC:\\pd\\Lodestar\\logs\r\n"
        );
        let s = parse_inspection(&text).unwrap();
        assert_eq!(s.folder.sddl, PRIVATE);
        assert!(!s.folder.link);
        assert_eq!(s.entries.len(), 3);
        assert_eq!(s.entries[0].name, "config.toml");
        assert!(s.entries[1].link);
        assert_eq!(s.entries[2].name, "name\twith tab");
        assert_eq!(s.links, [r"C:\pd\Lodestar\logs"]);
        assert!(parse_inspection("").is_err());
        assert!(parse_inspection("E\tArchive\tO:BAD:\tx\n").is_err());
        assert!(parse_inspection("F\ta\tb\n").is_err());
        assert!(parse_inspection("F\ta\tb\tc\nF\ta\tb\tc\n").is_err());
    }

    /// Fake [`Ops`]: a folder whose inspections are scripted in order.
    struct Fake {
        inspections: RefCell<Vec<Option<Inspection>>>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl Fake {
        fn new(inspections: Vec<Option<Inspection>>) -> Fake {
            Fake {
                inspections: RefCell::new(inspections),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Ops for Fake {
        fn inspect(&self, _: &Path) -> io::Result<Option<Inspection>> {
            self.calls.borrow_mut().push("inspect");
            Ok(self.inspections.borrow_mut().remove(0))
        }
        fn create(&self, _: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push("create");
            Ok(())
        }
    }

    fn secure(fake: &Fake) -> Result<(), String> {
        secure_dir(Path::new("Lodestar"), Some(OWN), fake)
    }

    #[test]
    fn creates_a_missing_folder_and_checks_it() {
        let fake = Fake::new(vec![None, Some(seen(PRIVATE, &[]))]);
        assert_eq!(secure(&fake), Ok(()));
        assert_eq!(*fake.calls.borrow(), ["inspect", "create", "inspect"]);
        // What's there after the create is judged like any folder.
        let fake = Fake::new(vec![None, Some(seen(INHERITED, &[]))]);
        assert!(secure(&fake).unwrap_err().contains("isn't private"));
    }

    #[test]
    fn a_private_folder_is_used_untouched() {
        let fake = Fake::new(vec![Some(seen(PRIVATE, &[item("auth.json", "O:SYD:AI")]))]);
        assert_eq!(secure(&fake), Ok(()));
        assert_eq!(*fake.calls.borrow(), ["inspect"]);
    }

    #[test]
    fn a_folder_that_isnt_private_is_refused_untouched() {
        // Foreign-owned and empty, then SYSTEM- or Administrators-owned
        // but open: both refused, nothing changed, the repair named.
        let admin_open = "O:SYD:AI(A;OICIID;FA;;;SY)(A;OICIID;0x1200a9;;;BU)";
        for folder in [INHERITED, admin_open] {
            let fake = Fake::new(vec![Some(seen(folder, &[]))]);
            let err = secure(&fake).unwrap_err();
            assert!(
                err.contains("isn't private") && err.contains("Repair"),
                "{err}"
            );
            assert_eq!(*fake.calls.borrow(), ["inspect"]);
        }
    }

    #[test]
    fn reads_the_service_sid() {
        let en = "\r\nNAME: Lodestar\r\nSERVICE SID: S-1-5-80-3961433548-1406390012-1291474617-2826939337-1520367402\r\nSTATUS: Active\r\n";
        assert_eq!(
            parse_service_sid(en).as_deref(),
            Some("S-1-5-80-3961433548-1406390012-1291474617-2826939337-1520367402")
        );
        let de = "NAME: Lodestar\nDIENST-SID: S-1-5-80-1-2-3-4-5\nSTATUS: Inaktiv\n";
        assert_eq!(parse_service_sid(de).as_deref(), Some(OWN));
        assert_eq!(parse_service_sid("S-1-5-80-"), None);
        assert_eq!(parse_service_sid("S-1-5-18 S-1-5-80-x"), None);
        assert_eq!(parse_service_sid(""), None);
    }

    /// Needs an administrator (CI's runner is one), like the service test.
    #[cfg(windows)]
    fn admin() -> bool {
        std::env::var_os("CI").is_some() || std::env::var_os("LODESTAR_SERVICE_TEST").is_some()
    }

    #[cfg(windows)]
    fn run(program: &str, args: &[&std::ffi::OsStr]) -> String {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[cfg(windows)]
    #[test]
    fn creates_and_refuses_for_real() {
        if !admin() {
            return;
        }
        // A service every Windows has stands in for Lodestar's, so icacls
        // can name it.
        let sid = service_sid("EventLog").unwrap();
        assert!(sid.starts_with("S-1-5-80-"), "{sid}");
        let ops = Windows {
            own_sid: Some(&sid),
        };
        let tmp = tempfile::tempdir().unwrap();
        let sddl_of = |p: &Path| ops.inspect(p).unwrap().unwrap().folder.sddl;

        // Created with its final ACL.
        let dir = tmp.path().join("created");
        secure_dir(&dir, Some(&sid), &ops).unwrap();
        let seen = ops.inspect(&dir).unwrap().unwrap();
        assert_eq!(verdict(&seen, Some(&sid)), Ok(()), "{seen:?}");
        assert!(seen.folder.sddl.contains(&sid), "{seen:?}");
        std::fs::write(dir.join("auth.json"), "{}").unwrap();
        let acl = run("icacls.exe", &[dir.join("auth.json").as_os_str()]);
        assert!(!acl.contains("BUILTIN\\Users"), "{acl}");
        assert!(acl.contains("NT SERVICE\\EventLog"), "{acl}");
        secure_dir(&dir, Some(&sid), &ops).unwrap();
        // Another service's SID isn't this one's.
        let other = service_sid("LodestarOther").unwrap();
        let other_ops = Windows {
            own_sid: Some(&other),
        };
        assert!(secure_dir(&dir, Some(&other), &other_ops).is_err());

        // An inherited, empty folder is refused, and left as it was.
        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        let before = sddl_of(&open);
        let err = secure_dir(&open, Some(&sid), &ops).unwrap_err();
        assert!(err.contains("inherits"), "{err}");
        assert_eq!(sddl_of(&open), before);

        // So is a folder owned by someone else (Users here) with a
        // private-looking ACL, empty or not.
        let foreign = tmp.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        let args = |a: &[&str]| -> Vec<std::ffi::OsString> {
            std::iter::once(foreign.clone().into_os_string())
                .chain(a.iter().map(|s| s.into()))
                .collect()
        };
        let icacls = |a: Vec<std::ffi::OsString>| {
            let refs: Vec<&std::ffi::OsStr> = a.iter().map(|s| s.as_os_str()).collect();
            run("icacls.exe", &refs);
        };
        icacls(args(&[
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
        ]));
        icacls(args(&["/setowner", "*S-1-5-32-545"]));
        let before = sddl_of(&foreign);
        let err = secure_dir(&foreign, Some(&sid), &ops).unwrap_err();
        assert!(err.contains("belongs to BU"), "{err}");
        assert_eq!(sddl_of(&foreign), before);
        std::fs::write(foreign.join("config.toml"), "").unwrap();
        assert!(secure_dir(&foreign, Some(&sid), &ops).is_err());

        // And a private folder holding an entry someone else owns.
        let planted = tmp.path().join("planted");
        secure_dir(&planted, Some(&sid), &ops).unwrap();
        std::fs::write(planted.join("config.toml"), "").unwrap();
        run(
            "icacls.exe",
            &[
                planted.join("config.toml").as_os_str(),
                "/setowner".as_ref(),
                "*S-1-5-32-545".as_ref(),
            ],
        );
        let err = secure_dir(&planted, Some(&sid), &ops).unwrap_err();
        assert!(err.contains("belongs to"), "{err}");
    }

    /// Creating over a path that became a junction (a race) changes
    /// nothing behind it, and the inspection then refuses the link.
    #[cfg(windows)]
    #[test]
    fn a_create_never_reaches_through_a_junction() {
        if !admin() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let junction = tmp.path().join("Lodestar");
        run(
            "cmd",
            &[
                "/C".as_ref(),
                "mklink".as_ref(),
                "/J".as_ref(),
                junction.as_os_str(),
                target.as_os_str(),
            ],
        );
        let ops = Windows { own_sid: None };
        let before = ops.inspect(&target).unwrap().unwrap().folder.sddl;
        let _ = ops.create(&junction);
        assert_eq!(ops.inspect(&target).unwrap().unwrap().folder.sddl, before);
        assert!(
            secure_dir(&junction, None, &ops)
                .unwrap_err()
                .contains("link or junction")
        );
    }

    #[cfg(windows)]
    #[test]
    fn links_are_refused_for_real() {
        if !admin() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let ops = Windows { own_sid: None };
        // The folder itself.
        let junction = tmp.path().join("Lodestar");
        run(
            "cmd",
            &[
                "/C".as_ref(),
                "mklink".as_ref(),
                "/J".as_ref(),
                junction.as_os_str(),
                target.as_os_str(),
            ],
        );
        let err = secure_dir(&junction, None, &ops).unwrap_err();
        assert!(err.contains("link or junction"), "{err}");
        // A junction inside a private folder.
        let dir = tmp.path().join("private");
        secure_dir(&dir, None, &ops).unwrap();
        run(
            "cmd",
            &[
                "/C".as_ref(),
                "mklink".as_ref(),
                "/J".as_ref(),
                dir.join("logs").as_os_str(),
                target.as_os_str(),
            ],
        );
        let err = secure_dir(&dir, None, &ops).unwrap_err();
        assert!(err.contains("is a link or junction inside it"), "{err}");
    }
}
