//! Locking down the service's default data folder (architecture §11,
//! §12). `%ProgramData%` lets every local user read and create folders and
//! files, and a new folder inherits that. Lodestar keeps its session key
//! (`auth.json`), its log (with the setup token) and its database there,
//! and the service reads `config.toml` from it, so before anything is read
//! or written the folder gets an ACL of its own: full control for
//! `SYSTEM`, `Administrators` and the service's own SID, nothing
//! inherited, nothing for `Users`.
//!
//! The first time (no [`MARKER`] yet) the whole tree is taken over: owner
//! Administrators, inherited ACLs only, then the folder's protected ACL.
//! A folder that already existed and holds anything is refused: a user
//! could have planted a `config.toml` or a junction there before Lodestar
//! secured it. Later starts re-apply the folder's ACL only.
//!
//! The policy is plain code tested on every OS; [`icacls`] is the Windows
//! side. The workspace forbids `unsafe`, so the ACL is set through
//! `icacls` by absolute path, as the secrets key's is.

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

/// Creates or secures `dir` through `lock`. An error says why the
/// service won't use the folder.
pub fn secure_dir(dir: &Path, lock: impl Fn(&Path, Lock) -> io::Result<()>) -> Result<(), String> {
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
    if !created && marker.exists() {
        return lock(dir, Lock::Folder).map_err(|e| format!("securing {shown}: {e}"));
    }
    lock(dir, Lock::Tree).map_err(|e| format!("securing {shown}: {e}"))?;
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
    fs::write(&marker, "Lodestar set this folder's permissions.\r\n")
        .map_err(|e| format!("writing {}: {e}", marker.display()))
}

/// The `icacls` runs for `lock` on `dir`, each a list of arguments after
/// the path. `service` is the service's name, for its SID
/// (`NT SERVICE\<name>`).
pub fn icacls_steps(lock: Lock, service: Option<&str>) -> Vec<Vec<String>> {
    let mut grant = vec![
        "/inheritance:r".to_string(),
        "/grant:r".to_string(),
        "*S-1-5-18:(OI)(CI)F".to_string(),
        "*S-1-5-32-544:(OI)(CI)F".to_string(),
    ];
    if let Some(name) = service {
        grant.push(format!("NT SERVICE\\{name}:(OI)(CI)F"));
    }
    let own = ["/setowner", "*S-1-5-32-544", "/T", "/C", "/L", "/Q"];
    let reset = ["/reset", "/T", "/C", "/L", "/Q"];
    let strings = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
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
pub fn icacls(dir: &Path, lock: Lock, service: Option<&str>) -> io::Result<()> {
    for args in icacls_steps(lock, service) {
        let mut cmd = super::system32_command("icacls.exe")
            .ok_or_else(|| io::Error::other("SystemRoot isn't set"))?;
        let out = cmd.arg(dir).args(&args).output()?;
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn run(dir: &Path) -> (Result<(), String>, Vec<Lock>) {
        let calls = RefCell::new(Vec::new());
        let r = secure_dir(dir, |_, l| {
            calls.borrow_mut().push(l);
            Ok(())
        });
        (r, calls.into_inner())
    }

    #[test]
    fn a_new_folder_is_created_secured_and_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Lodestar");
        assert_eq!(run(&dir), (Ok(()), vec![Lock::Tree]));
        assert!(dir.join(MARKER).exists());
        // Later starts only re-apply the folder's ACL.
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
    fn a_failed_lock_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let r = secure_dir(tmp.path(), |_, _| Err(io::Error::other("denied")));
        assert!(r.unwrap_err().contains("denied"));
        assert!(!tmp.path().join(MARKER).exists());
        fs::write(tmp.path().join(MARKER), "").unwrap();
        let r = secure_dir(tmp.path(), |_, _| Err(io::Error::other("denied")));
        assert!(r.unwrap_err().contains("denied"));
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
        let tree = icacls_steps(Lock::Tree, Some("Lodestar"));
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
                "NT SERVICE\\Lodestar:(OI)(CI)F"
            ]
        );
        assert_eq!(
            icacls_steps(Lock::Folder, None),
            vec![tree[2][..4].to_vec()]
        );
    }

    /// Needs an administrator (CI's runner is one), like the service test.
    #[cfg(windows)]
    fn admin() -> bool {
        std::env::var_os("CI").is_some() || std::env::var_os("LODESTAR_SERVICE_TEST").is_some()
    }

    #[cfg(windows)]
    fn acl(path: &Path) -> String {
        let out = std::process::Command::new("icacls.exe")
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[cfg(windows)]
    #[test]
    fn icacls_leaves_only_system_and_administrators() {
        if !admin() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Lodestar");
        secure_dir(&dir, |d, l| icacls(d, l, None)).unwrap();
        fs::write(dir.join("auth.json"), "{}").unwrap();
        for path in [dir.clone(), dir.join("auth.json")] {
            let acl = acl(&path);
            assert!(acl.contains("NT AUTHORITY\\SYSTEM"), "{acl}");
            assert!(acl.contains("BUILTIN\\Administrators"), "{acl}");
            assert!(!acl.contains("BUILTIN\\Users"), "{acl}");
            assert!(!acl.contains("Authenticated Users"), "{acl}");
            assert!(!acl.contains("CREATOR OWNER"), "{acl}");
        }
        // The folder's own entries aren't inherited.
        assert!(!acl(&dir).contains("(I)"), "{}", acl(&dir));
        // A second start re-applies it.
        secure_dir(&dir, |d, l| icacls(d, l, None)).unwrap();
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
        let r = secure_dir(&junction, |d, l| icacls(d, l, None));
        assert!(r.unwrap_err().contains("link or junction"));
    }
}
