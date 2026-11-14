//! Who listens on a managed companion's loopback port (X-cmp09-fix).
//!
//! Before a run is marked ready ([`super::bind::Bindings::try_ready`]),
//! every listening socket that accepts connections to `127.0.0.1` on the
//! port must belong to the process the supervisor started for that run:
//! - **Linux:** the socket's inode, from `/proc/net/tcp` and `tcp6`, must
//!   be among the open sockets of the child or a process descended from it
//!   (Lodestar's launcher `exec`s, so the child's pid is the companion's).
//! - **Windows:** each listener's owning process, from
//!   `Get-NetTCPConnection`, must be the child itself. The companion is
//!   spawned directly there (the launcher is Linux-only), and it listens
//!   in its own process.
//! - **Anywhere else:** it can't be told, so it's never ready. No native
//!   companion is offered there.
//!
//! What can't be read or parsed is [`Owner::Unknown`], which never counts:
//! it fails closed.

/// The answer for one port and one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// Every listener that accepts `127.0.0.1` on the port is the
    /// process's (or, on Linux, a descendant's).
    Ours,
    /// At least one such listener belongs to another process.
    Other,
    /// Nothing listens there.
    Nobody,
    /// It couldn't be told.
    Unknown,
}

impl Owner {
    /// The code a refusal records ([`super::bind::Bindings::refusal`]).
    pub fn code(self) -> &'static str {
        match self {
            Owner::Ours => "listener_ours",
            Owner::Other => "listener_not_ours",
            Owner::Nobody => "listener_missing",
            Owner::Unknown => "listener_unverified",
        }
    }
}

/// Who listens on `127.0.0.1:port`, compared with process `pid`. It
/// blocks (it reads `/proc`, or runs PowerShell): call it from a blocking
/// thread.
pub fn owner(port: u16, pid: u32) -> Owner {
    imp::owner(port, pid)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashSet;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::Path;

    use super::Owner;

    pub(super) fn owner(port: u16, pid: u32) -> Owner {
        owner_in(Path::new("/proc"), port, pid)
    }

    /// The same against another `/proc` (tests).
    pub(super) fn owner_in(proc: &Path, port: u16, pid: u32) -> Owner {
        let mut inodes = Vec::new();
        for (file, v6) in [("net/tcp", false), ("net/tcp6", true)] {
            match fs::read_to_string(proc.join(file)) {
                Ok(text) => match super::linux::listeners(&text, v6, port) {
                    Some(found) => inodes.extend(found),
                    None => return Owner::Unknown,
                },
                // A kernel without IPv6 has no tcp6.
                Err(e) if v6 && e.kind() == ErrorKind::NotFound => {}
                Err(_) => return Owner::Unknown,
            }
        }
        if inodes.is_empty() {
            return Owner::Nobody;
        }
        let Some(ours) = group_sockets(proc, pid) else {
            return Owner::Unknown;
        };
        if inodes.iter().all(|i| ours.contains(i)) {
            Owner::Ours
        } else {
            Owner::Other
        }
    }

    /// The socket inodes open in `pid` and every process descended from
    /// it (by parent, not by process group, which another process of the
    /// same session could join). `None` when the child's own descriptors
    /// can't be read, or a descendant's can't be for any reason but its
    /// having exited.
    fn group_sockets(proc: &Path, pid: u32) -> Option<HashSet<u64>> {
        let mut parents = std::collections::HashMap::new();
        for entry in fs::read_dir(proc).ok()?.flatten() {
            let Some(n) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if n == pid {
                continue;
            }
            // One that exited meanwhile has no stat: it's not counted.
            let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            if let Some(parent) = super::linux::parent(&stat) {
                parents.insert(n, parent);
            }
        }
        let mut members = vec![pid];
        members.extend(
            parents
                .keys()
                .copied()
                .filter(|&n| super::linux::descends(&parents, n, pid)),
        );
        let mut sockets = HashSet::new();
        for m in members {
            let fds = match fs::read_dir(proc.join(m.to_string()).join("fd")) {
                Ok(fds) => fds,
                Err(e) if m != pid && e.kind() == ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            for fd in fds.flatten() {
                // A descriptor closed meanwhile has no link.
                let Ok(link) = fs::read_link(fd.path()) else {
                    continue;
                };
                if let Some(inode) = super::linux::socket_inode(&link.to_string_lossy()) {
                    sockets.insert(inode);
                }
            }
        }
        Some(sockets)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn an_unreadable_proc_is_unknown() {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(owner_in(dir.path(), 8080, 1), Owner::Unknown);
        }

        #[test]
        fn a_child_whose_descriptors_cant_be_read_is_unknown() {
            let dir = tempfile::tempdir().unwrap();
            let net = dir.path().join("net");
            fs::create_dir(&net).unwrap();
            let row = "  0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0 100 0 0 10 0\n";
            fs::write(
                net.join("tcp"),
                format!("{}{row}", super::super::linux::HEADER),
            )
            .unwrap();
            // No /proc/<pid>/fd for the child.
            assert_eq!(owner_in(dir.path(), 8080, 77), Owner::Unknown);
            // Nothing on another port.
            assert_eq!(owner_in(dir.path(), 8081, 77), Owner::Nobody);
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::Owner;

    pub(super) fn owner(port: u16, pid: u32) -> Owner {
        // The last line says the script ran to its end, so no listeners
        // is told apart from a failed call.
        // No match is an error of category ObjectNotFound; any other error
        // (the cmdlet or its module unavailable) prints no `end`, so it
        // reads as Unknown. No double quotes, which argument quoting could
        // mangle.
        let script = format!(
            "$c = @(Get-NetTCPConnection -State Listen -LocalPort {port} \
             -ErrorAction SilentlyContinue -ErrorVariable e); \
             if (@($e | Where-Object {{ $_.CategoryInfo.Category -ne 'ObjectNotFound' }}).Count) \
             {{ 'failed'; exit }}; \
             $c | ForEach-Object {{ [string]$_.LocalAddress + ' ' + [string]$_.OwningProcess }}; 'end'"
        );
        match crate::supervise::pidfile::windows::powershell(&script) {
            Some(out) => super::windows::judge(&out, pid),
            None => Owner::Unknown,
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::Owner;

    pub(super) fn owner(_port: u16, _pid: u32) -> Owner {
        Owner::Unknown
    }
}

/// Parsing `/proc/net/tcp`, `tcp6` and `/proc/<pid>/stat`: pure, and
/// tested on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux {
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// The header line of `/proc/net/tcp`.
    #[cfg(test)]
    pub(super) const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";

    /// The `st` of a listening socket.
    const LISTEN: &str = "0A";

    /// The inodes of the listening sockets in a `/proc/net/tcp` (or, with
    /// `v6`, `tcp6`) table that accept connections to `127.0.0.1:port`.
    /// `None` when a row can't be read.
    pub(super) fn listeners(table: &str, v6: bool, port: u16) -> Option<Vec<u64>> {
        let mut found = Vec::new();
        for line in table.lines().skip(1) {
            if line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            let (local, st, inode) = (f.get(1)?, f.get(3)?, f.get(9)?);
            if *st != LISTEN {
                continue;
            }
            let (addr, p) = local.split_once(':')?;
            if u16::from_str_radix(p, 16).ok()? != port {
                continue;
            }
            let accepts = if v6 {
                accepts_v6(parse_v6(addr)?)
            } else {
                accepts_v4(parse_v4(addr)?)
            };
            if accepts {
                let inode: u64 = inode.parse().ok()?;
                // A listening socket always has one.
                if inode == 0 {
                    return None;
                }
                found.push(inode);
            }
        }
        Some(found)
    }

    /// An address as the kernel prints it: each 32-bit word in the host's
    /// byte order, as eight hex digits.
    fn word(hex: &str) -> Option<[u8; 4]> {
        if hex.len() != 8 {
            return None;
        }
        u32::from_str_radix(hex, 16).ok().map(u32::to_ne_bytes)
    }

    fn parse_v4(hex: &str) -> Option<Ipv4Addr> {
        word(hex).map(Ipv4Addr::from)
    }

    fn parse_v6(hex: &str) -> Option<Ipv6Addr> {
        if hex.len() != 32 {
            return None;
        }
        let mut b = [0u8; 16];
        for i in 0..4 {
            b[i * 4..i * 4 + 4].copy_from_slice(&word(hex.get(i * 8..i * 8 + 8)?)?);
        }
        Some(Ipv6Addr::from(b))
    }

    /// Whether a listener bound here takes connections to `127.0.0.1`.
    pub(super) fn accepts_v4(a: Ipv4Addr) -> bool {
        a == Ipv4Addr::LOCALHOST || a.is_unspecified()
    }

    /// The same for an IPv6 listener: the wildcard (which takes IPv4 too
    /// unless it's v6-only, which can't be seen here, so it counts) and
    /// the mapped forms of the two above.
    pub(super) fn accepts_v6(a: Ipv6Addr) -> bool {
        a.is_unspecified() || a.to_ipv4_mapped().is_some_and(accepts_v4)
    }

    /// The parent in a `/proc/<pid>/stat` line: the second field after the
    /// command name, which is in parentheses and may hold anything, so it's
    /// read from the last `)`.
    pub(super) fn parent(stat: &str) -> Option<u32> {
        let rest = &stat[stat.rfind(')')? + 1..];
        rest.split_whitespace().nth(1)?.parse().ok()
    }

    /// Whether `n` descends from `root` by its parents. The walk is bounded,
    /// so a table read mid-change can't loop.
    pub(super) fn descends(parents: &HashMap<u32, u32>, n: u32, root: u32) -> bool {
        let mut at = n;
        for _ in 0..64 {
            match parents.get(&at) {
                Some(&p) if p == root => return true,
                Some(&p) if p != at && p != 0 => at = p,
                _ => return false,
            }
        }
        false
    }

    /// The inode of a `socket:[N]` descriptor link.
    pub(super) fn socket_inode(link: &str) -> Option<u64> {
        link.strip_prefix("socket:[")?
            .strip_suffix(']')?
            .parse()
            .ok()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn row(local: &str, st: &str, inode: u64) -> String {
            format!(
                "   0: {local} 00000000:0000 {st} 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0\n"
            )
        }

        fn hex4(a: Ipv4Addr) -> String {
            format!("{:08X}", u32::from_ne_bytes(a.octets()))
        }

        fn hex6(a: Ipv6Addr) -> String {
            a.octets()
                .chunks(4)
                .map(|w| format!("{:08X}", u32::from_ne_bytes([w[0], w[1], w[2], w[3]])))
                .collect()
        }

        #[test]
        fn ipv4_listeners_that_take_loopback() {
            let t = [
                HEADER.to_owned(),
                row(&format!("{}:1F90", hex4(Ipv4Addr::LOCALHOST)), "0A", 11),
                row(&format!("{}:1F90", hex4(Ipv4Addr::UNSPECIFIED)), "0A", 12),
                // Another loopback address, another port, not listening.
                row(
                    &format!("{}:1F90", hex4(Ipv4Addr::new(127, 0, 0, 2))),
                    "0A",
                    13,
                ),
                row(&format!("{}:1F91", hex4(Ipv4Addr::LOCALHOST)), "0A", 14),
                row(&format!("{}:1F90", hex4(Ipv4Addr::LOCALHOST)), "01", 15),
            ]
            .concat();
            assert_eq!(listeners(&t, false, 8080), Some(vec![11, 12]));
            assert_eq!(listeners(&t, false, 8081), Some(vec![14]));
            assert_eq!(listeners(&t, false, 9), Some(vec![]));
        }

        #[test]
        fn ipv6_listeners_that_take_loopback() {
            let mapped: Ipv6Addr = "::ffff:127.0.0.1".parse().unwrap();
            let t = [
                HEADER.to_owned(),
                row(&format!("{}:1F90", hex6(Ipv6Addr::UNSPECIFIED)), "0A", 21),
                row(&format!("{}:1F90", hex6(mapped)), "0A", 22),
                // ::1 doesn't take 127.0.0.1.
                row(&format!("{}:1F90", hex6(Ipv6Addr::LOCALHOST)), "0A", 23),
            ]
            .concat();
            assert_eq!(listeners(&t, true, 8080), Some(vec![21, 22]));
        }

        #[test]
        fn a_table_that_cant_be_read_is_none() {
            // An address that isn't hex, a row cut short, a listener with
            // no inode.
            let bad = [
                format!("{HEADER}   0: zz:1F90 00000000:0000 0A x x x 0 0 5\n"),
                format!("{HEADER}   0: 0100007F:1F90 00000000:0000 0A\n"),
                format!("{HEADER}{}", row("0100007F:1F90", "0A", 0)),
            ];
            for t in &bad {
                assert_eq!(listeners(t, false, 8080), None, "{t}");
            }
        }

        #[test]
        fn the_parent_is_read_after_the_name() {
            assert_eq!(parent("42 (a) b) S 7 77 77 0 -1"), Some(7));
            assert_eq!(parent("42 (qbittorrent-nox) S 1 42 42 0"), Some(1));
            assert_eq!(parent("garbage"), None);
            // 50 → 45 → 40: descends from 40, not from 41.
            let parents = HashMap::from([(50, 45), (45, 40), (40, 1), (60, 1)]);
            assert!(descends(&parents, 50, 40) && descends(&parents, 45, 40));
            assert!(!descends(&parents, 60, 40) && !descends(&parents, 50, 41));
            // A loop in a table read mid-change ends.
            let looped = HashMap::from([(5, 6), (6, 5)]);
            assert!(!descends(&looped, 5, 40));
            assert_eq!(socket_inode("socket:[4242]"), Some(4242));
            assert_eq!(socket_inode("pipe:[1]"), None);
        }
    }
}

/// Reading `Get-NetTCPConnection`'s lines: pure, and tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
mod windows {
    use std::net::IpAddr;

    use super::Owner;

    /// One `<address> <pid>` line per listener on the port, then `end`.
    pub(super) fn judge(out: &str, pid: u32) -> Owner {
        let mut lines: Vec<&str> = out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if lines.pop() != Some("end") {
            return Owner::Unknown;
        }
        let mut any = false;
        for line in lines {
            let Some((addr, owner)) = line.split_once(' ') else {
                return Owner::Unknown;
            };
            let (Ok(addr), Ok(owner)) = (addr.parse::<IpAddr>(), owner.trim().parse::<u32>())
            else {
                return Owner::Unknown;
            };
            let accepts = match addr {
                IpAddr::V4(a) => super::linux::accepts_v4(a),
                IpAddr::V6(a) => super::linux::accepts_v6(a),
            };
            if !accepts {
                continue;
            }
            any = true;
            if owner != pid {
                return Owner::Other;
            }
        }
        if any { Owner::Ours } else { Owner::Nobody }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn listeners_are_judged_by_their_owner() {
            assert_eq!(judge("127.0.0.1 40\nend\n", 40), Owner::Ours);
            assert_eq!(judge("0.0.0.0 40\n:: 40\r\nend\r\n", 40), Owner::Ours);
            assert_eq!(judge("127.0.0.1 40\n0.0.0.0 41\nend", 40), Owner::Other);
            assert_eq!(judge("::ffff:127.0.0.1 41\nend", 40), Owner::Other);
            // ::1 and another address don't take 127.0.0.1.
            assert_eq!(judge("::1 41\n10.0.0.5 41\nend", 40), Owner::Nobody);
            assert_eq!(judge("end", 40), Owner::Nobody);
            // A call that didn't finish, or lines that don't parse.
            assert_eq!(judge("127.0.0.1 40\n", 40), Owner::Unknown);
            assert_eq!(judge("", 40), Owner::Unknown);
            assert_eq!(judge("localhost 40\nend", 40), Owner::Unknown);
            assert_eq!(judge("127.0.0.1 x\nend", 40), Owner::Unknown);
        }
    }
}
