//! Locking down the service's default data folder (architecture §11,
//! §12). `%ProgramData%` lets every local user read and create folders and
//! files, and a new folder inherits that. Lodestar keeps its session key
//! (`auth.json`), its log (with the setup token) and its database there,
//! and the service reads `config.toml` from it, so before anything is read
//! or written the folder must be private: a protected ACL (nothing
//! inherited) that lets only `SYSTEM`, `Administrators` and the service's
//! own SID (`NT SERVICE\<name>`) in. A shared account such as LocalService
//! isn't enough: many other services run as it.
//!
//! The installer (P-release) creates the folder that way and runs the
//! service as `NT SERVICE\Lodestar`, which can't take over a tree. So the
//! service:
//! 1. sets the ACL where its account may (LocalSystem, for example under
//!    `sc create`'s default). The first time (no [`MARKER`] yet) that takes
//!    over the whole tree: owner Administrators, inherited-only children,
//!    then the folder's protected ACL; later starts re-apply the folder's;
//! 2. always verifies the result from the folder's SDDL, which isn't
//!    localised, and refuses a folder that isn't private. Its own SID
//!    comes from `sc showsid`;
//! 3. the first time, refuses a folder that holds anything: a user could
//!    have planted a `config.toml` or a junction there before it was
//!    secured.
//!
//! The policy and the SDDL check are plain code tested on every OS;
//! [`icacls`], [`saved_sddl`] and [`service_sid`] are the Windows side. The workspace forbids
//! `unsafe`, so both run `icacls` by absolute path, as the secrets key's ACL
//! does.

use std::fs;
use std::io;
use std::path::Path;

/// Written once the folder has been secured.
pub const MARKER: &str = ".lodestar-secured";

/// How much [`secure_dir`] asks the locker to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    /// Take over the tree: owner, inherited-only children, then the
    /// folder's protected ACL.
    Tree,
    /// Re-apply the folder's protected ACL.
    Folder,
}

/// Creates or secures `dir`: `lock` sets the ACL (best effort: the
/// service's account may not be allowed to), `sddl` reads the folder's
/// DACL back for [`check_private`] with `own_sid`, the service's SID. An
/// error says why the service won't use the folder.
pub fn secure_dir(
    dir: &Path,
    lock: impl Fn(&Path, Lock) -> io::Result<()>,
    sddl: impl Fn(&Path) -> io::Result<String>,
    own_sid: Option<&str>,
) -> Result<(), String> {
    let shown = dir.display();
    let created = match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(dir).map_err(|e| format!("creating {shown}: {e}"))?;
            true
        }
        Err(e) => return Err(format!("reading {shown}: {e}")),
        Ok(m) if m.file_type().is_symlink() => {
            return Err(format!(
                "{shown} is a link or junction; Lodestar only uses a real folder there"
            ));
        }
        Ok(m) if !m.is_dir() => return Err(format!("{shown} isn't a folder")),
        Ok(_) => false,
    };
    let marker = dir.join(MARKER);
    let first = created || !marker.exists();
    let locked = lock(dir, if first { Lock::Tree } else { Lock::Folder });
    let verified = sddl(dir)
        .map_err(|e| format!("reading its permissions: {e}"))
        .and_then(|s| check_private(&s, own_sid));
    if let Err(why) = verified {
        return Err(match locked {
            Ok(()) => format!("{shown} isn't private: {why}"),
            Err(e) => format!(
                "{shown} isn't private ({why}), and Lodestar couldn't secure it: {e}. \
                 Reinstall Lodestar, or give the folder the permissions the installer \
                 sets (full control for SYSTEM, Administrators and NT SERVICE\\Lodestar \
                 only, nothing inherited)."
            ),
        });
    }
    if !first {
        return Ok(());
    }
    let planted = fs::read_dir(dir)
        .map_err(|e| format!("reading {shown}: {e}"))?
        .filter_map(Result::ok)
        .any(|e| e.file_name() != MARKER);
    if planted {
        return Err(format!(
            "{shown} existed before Lodestar secured it and isn't empty, so another \
             user could have put files in it. Move its contents somewhere else, check \
             them, and start the service again (an empty folder is fine)."
        ));
    }
    fs::write(&marker, "Lodestar checked this folder's permissions.\r\n")
        .map_err(|e| format!("writing {}: {e}", marker.display()))
}

/// Checks a folder's DACL in SDDL (`D:PAI(A;OICI;FA;;;SY)…`): protected
/// (nothing inherited from `%ProgramData%`), and every allow entry for
/// SYSTEM, Administrators or `own_sid` (the service's SID). Any other
/// account, shared service accounts included, is refused. Deny entries
/// are fine.
pub fn check_private(sddl: &str, own_sid: Option<&str>) -> Result<(), String> {
    let trusted = |sid: &str| {
        matches!(sid, "SY" | "BA" | "S-1-5-18" | "S-1-5-32-544") || Some(sid) == own_sid
    };
    let dacl = sddl
        .find("D:")
        .map(|i| &sddl[i + 2..])
        .ok_or("no DACL in its security descriptor")?;
    let flags = &dacl[..dacl.find('(').unwrap_or(dacl.len())];
    if !flags.contains('P') {
        return Err("it inherits permissions from its parent folder".into());
    }
    let mut rest = &dacl[flags.len()..];
    while let Some(body) = rest.strip_prefix('(') {
        let end = body.find(')').ok_or("a malformed entry")?;
        let fields: Vec<&str> = body[..end].split(';').collect();
        let [kind, _, _, _, _, sid] = fields[..] else {
            return Err("a malformed entry".into());
        };
        if kind.starts_with('A') && !trusted(sid) {
            return Err(format!("it grants access to {sid}"));
        }
        rest = &body[end + 1..];
    }
    Ok(())
}

/// The `icacls` runs for `lock`, each a list of arguments after the path:
/// full control for SYSTEM, Administrators and `own_sid`, as the installer
/// sets them.
pub fn icacls_steps(lock: Lock, own_sid: Option<&str>) -> Vec<Vec<String>> {
    let strings = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut grant = strings(&[
        "/inheritance:r",
        "/grant:r",
        "*S-1-5-18:(OI)(CI)F",
        "*S-1-5-32-544:(OI)(CI)F",
    ]);
    grant.extend(own_sid.map(|sid| format!("*{sid}:(OI)(CI)F")));
    let own = ["/setowner", "*S-1-5-32-544", "/T", "/C", "/L", "/Q"];
    let reset = ["/reset", "/T", "/C", "/L", "/Q"];
    match lock {
        // Owner first, so the previous owner loses the right to change
        // the ACL; then explicit entries go (the reset), then the
        // inherited ones (the protected ACL, which the children inherit).
        Lock::Tree => vec![strings(&own), strings(&reset), grant],
        Lock::Folder => vec![grant],
    }
}

/// Runs [`icacls_steps`] with `%SystemRoot%\System32\icacls.exe`.
#[cfg(windows)]
pub fn icacls(dir: &Path, lock: Lock, own_sid: Option<&str>) -> io::Result<()> {
    for args in icacls_steps(lock, own_sid) {
        let out = icacls_command()?.arg(dir).args(&args).output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "icacls {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout).trim()
            )));
        }
    }
    Ok(())
}

/// The folder's DACL in SDDL, from `icacls <dir> /save` (a UTF-16 file:
/// the name, then the SDDL).
#[cfg(windows)]
pub fn saved_sddl(dir: &Path) -> io::Result<String> {
    // Created here first, exclusively: in a shared temp folder another
    // user could otherwise pre-create the file and swap its contents.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let file =
        std::env::temp_dir().join(format!("lodestar-acl-{}-{nanos}.txt", std::process::id()));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)?;
    let out = icacls_command()?
        .arg(dir)
        .arg("/save")
        .arg(&file)
        .arg("/Q")
        .output()?;
    let bytes = fs::read(&file);
    let _ = fs::remove_file(&file);
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "icacls /save: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        )));
    }
    let units: Vec<u16> = bytes?
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(String::from_utf16_lossy(&units))
}

/// The service SID in `sc showsid` output (its labels are translated;
/// the SID isn't).
pub fn parse_service_sid(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|w| {
            w.strip_prefix("S-1-5-80-")
                .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit() || c == '-'))
        })
        .map(String::from)
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

#[cfg(windows)]
fn icacls_command() -> io::Result<std::process::Command> {
    super::system32_command("icacls.exe").ok_or_else(|| io::Error::other("SystemRoot isn't set"))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// The service's own SID (`NT SERVICE\Lodestar`).
    const OWN: &str = "S-1-5-80-1-2-3-4-5";
    const PRIVATE: &str = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;S-1-5-80-1-2-3-4-5)";

    fn run_with(dir: &Path, sddl: &str) -> (Result<(), String>, Vec<Lock>) {
        let calls = RefCell::new(Vec::new());
        let r = secure_dir(
            dir,
            |_, l| {
                calls.borrow_mut().push(l);
                Ok(())
            },
            |_| Ok(sddl.to_string()),
            Some(OWN),
        );
        (r, calls.into_inner())
    }

    fn run(dir: &Path) -> (Result<(), String>, Vec<Lock>) {
        run_with(dir, PRIVATE)
    }

    #[test]
    fn a_new_folder_is_created_secured_and_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Lodestar");
        assert_eq!(run(&dir), (Ok(()), vec![Lock::Tree]));
        assert!(dir.join(MARKER).exists());
        // Later starts re-apply the folder's ACL only, and may hold files.
        fs::write(dir.join("auth.json"), "{}").unwrap();
        assert_eq!(run(&dir), (Ok(()), vec![Lock::Folder]));
    }

    #[test]
    fn an_empty_existing_folder_is_taken_over() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(run(tmp.path()), (Ok(()), vec![Lock::Tree]));
        assert!(tmp.path().join(MARKER).exists());
    }

    #[test]
    fn a_folder_with_contents_before_securing_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("config.toml"), "").unwrap();
        let (r, calls) = run(tmp.path());
        // Locked first (so nobody else can add to it), then refused.
        assert_eq!(calls, vec![Lock::Tree]);
        assert!(r.unwrap_err().contains("isn't empty"));
        assert!(!tmp.path().join(MARKER).exists());
    }

    #[test]
    fn an_account_that_cant_set_acls_relies_on_the_installers() {
        // The virtual account: icacls fails, the installer's ACL passes.
        let tmp = tempfile::tempdir().unwrap();
        let denied = |_: &Path, _| Err(io::Error::other("access denied"));
        let private = |_: &Path| Ok(PRIVATE.to_string());
        secure_dir(tmp.path(), denied, private, Some(OWN)).unwrap();
        assert!(tmp.path().join(MARKER).exists());
        secure_dir(tmp.path(), denied, private, Some(OWN)).unwrap();
        // A folder that isn't private is refused, saying both.
        let open = |_: &Path| Ok("D:AI(A;OICIID;FA;;;SY)(A;OICIID;0x1200a9;;;BU)".to_string());
        let err = secure_dir(tmp.path(), denied, open, Some(OWN)).unwrap_err();
        assert!(
            err.contains("inherits") && err.contains("access denied"),
            "{err}"
        );
        // So is one whose ACL can't be read.
        let unreadable = |_: &Path| Err(io::Error::other("no"));
        let err = secure_dir(tmp.path(), |_, _| Ok(()), unreadable, Some(OWN)).unwrap_err();
        assert!(err.contains("reading its permissions"), "{err}");
    }

    #[test]
    fn checks_the_dacl() {
        let check = |sddl: &str| check_private(sddl, Some(OWN));
        assert_eq!(check(PRIVATE), Ok(()));
        assert_eq!(
            check("D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)(D;;FA;;;BU)"),
            Ok(())
        );
        // As `icacls /save` writes it: the name first.
        assert_eq!(check(&format!("Lodestar\r\n{PRIVATE}\r\n")), Ok(()));
        assert!(
            check("D:AI(A;OICIID;FA;;;SY)")
                .unwrap_err()
                .contains("inherits")
        );
        assert!(
            check("D:P(A;OICI;FA;;;SY)(A;OICI;0x1301bf;;;BU)")
                .unwrap_err()
                .contains("BU")
        );
        // Shared service accounts and other services' SIDs aren't enough.
        for sddl in [
            "D:P(A;;FA;;;WD)",
            "D:P(A;;FA;;;AU)",
            "D:P(A;;FA;;;LS)",
            "D:P(A;;FA;;;S-1-5-19)",
            "D:P(A;;FA;;;NS)",
            "D:P(A;;FA;;;S-1-5-80-9-9-9-9-9)",
            "D:P(A;;FA;;;S-1-5-21-1-2-3-1001)",
        ] {
            assert!(check(sddl).is_err(), "{sddl}");
        }
        // Without its own SID, the service's entry isn't trusted either.
        assert!(check_private(PRIVATE, None).is_err());
        assert!(check("O:BA").is_err());
        assert!(check("D:P(A;;FA;;SY)").is_err());
        assert!(check("D:P(A;;FA;;;SY").is_err());
    }

    #[test]
    fn reads_the_service_sid() {
        let en = "\r\nNAME: Lodestar\r\nSERVICE SID: S-1-5-80-3961433548-1406390012-1291474617-2826939337-1520367402\r\nSTATUS: Active\r\n";
        assert_eq!(
            parse_service_sid(en).as_deref(),
            Some("S-1-5-80-3961433548-1406390012-1291474617-2826939337-1520367402")
        );
        let de = "NAME: Lodestar\nDIENST-SID: S-1-5-80-1-2-3-4-5\nSTATUS: Inaktiv\n";
        assert_eq!(parse_service_sid(de).as_deref(), Some("S-1-5-80-1-2-3-4-5"));
        assert_eq!(parse_service_sid("S-1-5-80-"), None);
        assert_eq!(parse_service_sid("S-1-5-18 S-1-5-80-x"), None);
        assert_eq!(parse_service_sid(""), None);
    }

    #[test]
    fn links_and_files_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(run(&file).0.unwrap_err().contains("isn't a folder"));
        #[cfg(unix)]
        {
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(tmp.path(), &link).unwrap();
            let (r, calls) = run(&link);
            assert!(r.unwrap_err().contains("link or junction"));
            assert!(calls.is_empty());
        }
    }

    #[test]
    fn icacls_takes_the_tree_then_protects_the_folder() {
        let tree = icacls_steps(Lock::Tree, Some(OWN));
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[0][..2], ["/setowner", "*S-1-5-32-544"]);
        assert_eq!(tree[1][0], "/reset");
        assert_eq!(
            tree[2],
            [
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
                "*S-1-5-80-1-2-3-4-5:(OI)(CI)F"
            ]
        );
        assert_eq!(icacls_steps(Lock::Folder, Some(OWN)), vec![tree[2].clone()]);
        assert_eq!(icacls_steps(Lock::Folder, None)[0].len(), 4);
    }

    /// Needs an administrator (CI's runner is one), like the service test.
    #[cfg(windows)]
    fn admin() -> bool {
        std::env::var_os("CI").is_some() || std::env::var_os("LODESTAR_SERVICE_TEST").is_some()
    }

    #[cfg(windows)]
    #[test]
    fn icacls_makes_the_folder_private() {
        if !admin() {
            return;
        }
        // `sc showsid` names any service's SID, installed or not.
        let sid = service_sid("Lodestar").unwrap();
        assert!(sid.starts_with("S-1-5-80-"), "{sid}");
        let own = Some(sid.as_str());
        let lock = |d: &Path, l| icacls(d, l, own);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Lodestar");
        // A fresh folder in a temp dir inherits its parent's ACL.
        fs::create_dir(&dir).unwrap();
        assert!(check_private(&saved_sddl(&dir).unwrap(), own).is_err());
        secure_dir(&dir, lock, saved_sddl, own).unwrap();
        let sddl = saved_sddl(&dir).unwrap();
        assert_eq!(check_private(&sddl, own), Ok(()), "{sddl}");
        assert!(sddl.contains(&sid), "{sddl}");
        fs::write(dir.join("auth.json"), "{}").unwrap();
        let out = std::process::Command::new("icacls.exe")
            .arg(dir.join("auth.json"))
            .output()
            .unwrap();
        let acl = String::from_utf8_lossy(&out.stdout);
        assert!(!acl.contains("BUILTIN\\Users"), "{acl}");
        assert!(acl.contains("NT AUTHORITY\\SYSTEM"), "{acl}");
        assert!(acl.contains("NT SERVICE\\Lodestar"), "{acl}");
        // A second start re-applies and re-checks it.
        secure_dir(&dir, lock, saved_sddl, own).unwrap();
        // Another service's SID isn't this one's.
        let other = service_sid("LodestarOther").unwrap();
        assert!(secure_dir(&dir, |_, _| Ok(()), saved_sddl, Some(&other)).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_is_refused() {
        if !admin() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let junction = tmp.path().join("Lodestar");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let r = secure_dir(&junction, |d, l| icacls(d, l, None), saved_sddl, None);
        assert!(r.unwrap_err().contains("link or junction"));
    }
}
