//! The binary's [`LogSource`]: `/logs` over the runtime's ring buffer and
//! log folder. `Services::run` layers it as an `Extension` and closes it
//! at shutdown.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use lodestar_api::logs::{
    Download, HistoryQuery, LineFilter, LogFileInfo, LogFileSkipped, LogFileState, LogFiles,
    LogFilesDeleted, LogFilesWhich, LogFormat, LogHistory, LogPurge, LogSource, LogStreamConfig,
    LogsError, RingRead, Transport,
};
use tokio::sync::watch;

use super::files::{self, Folder, IndexView, Listed, content, download_name, open_plain};
use super::format::Stamp;
use super::names::LogName;
use super::view::{data_kind, matches, ring_line};
use super::zip::{Opener, ZipStream};
use super::{FileStatus, Runtime, events};

/// A file's size and modification time, and its first line's time.
type Seen = (u64, Option<SystemTime>, Option<String>);

/// `/logs` over a runtime.
pub struct Logs {
    rt: Option<&'static Runtime>,
    transport: Transport,
    /// Each file's first line's time, by name, size and modification time.
    from: Mutex<HashMap<String, Seen>>,
}

impl std::fmt::Debug for Logs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logs").finish_non_exhaustive()
    }
}

/// The process's `/logs` source, over the installed runtime (none: every
/// route but the catalogue answers `503`).
pub fn source() -> Arc<dyn LogSource> {
    Arc::new(Logs::new(super::runtime(), LogStreamConfig::default()))
}

impl Logs {
    pub fn new(rt: Option<&'static Runtime>, config: LogStreamConfig) -> Logs {
        Logs {
            rt,
            transport: Transport::new(config),
            from: Mutex::default(),
        }
    }

    fn rt(&self) -> Result<&'static Runtime, LogsError> {
        self.rt.ok_or(LogsError::Unavailable)
    }

    fn folder(&self) -> Result<Folder, &'static str> {
        let Some(rt) = self.rt else {
            return Err("not_opened");
        };
        files::folder(rt.opened_folder(), &rt.configured_folder())
    }

    /// The listed regular files and the index, or `Unavailable`.
    fn listing(&self) -> Result<(Folder, Vec<Listed>, IndexView), LogsError> {
        let folder = self.folder().map_err(|_| LogsError::Unavailable)?;
        let listed = files::list(&folder.dir).map_err(LogsError::Internal)?;
        let index = files::read_index(&folder.dir);
        Ok((folder, listed, index))
    }

    fn opener(dir: &std::path::Path, name: &LogName, format: LogFormat, redact: bool) -> Opener {
        let dir = dir.to_path_buf();
        let name = name.clone();
        Box::new(move || Ok(content(open_plain(&dir, &name)?, format, redact)))
    }
}

impl LogSource for Logs {
    fn transport(&self) -> &Transport {
        &self.transport
    }

    fn epoch(&self) -> &str {
        self.rt.map_or("0000000000000000", |rt| rt.epoch())
    }

    fn bounds(&self) -> (u64, u64) {
        self.rt.map_or((0, 1), |rt| rt.ring().bounds())
    }

    fn listen(&self) -> (Box<dyn Send + Sync>, watch::Receiver<u64>) {
        match self.rt {
            Some(rt) => {
                let (guard, rx) = rt.ring().listen();
                (Box::new(guard), rx)
            }
            None => (Box::new(()), watch::channel(0).1),
        }
    }

    fn after(&self, cursor: u64, filter: &LineFilter, max: usize) -> RingRead {
        let Some(rt) = self.rt else {
            return RingRead::default();
        };
        let mask = rt.mask_now();
        let epoch = rt.epoch();
        let read = rt.ring().after(cursor, max, |e| {
            ring_line(e, mask, epoch).filter(|l| matches(l, filter))
        });
        RingRead {
            lines: read.lines,
            matching: read.matching,
            evicted: read.evicted,
            head: read.head,
        }
    }

    fn generation(&self) -> u64 {
        self.rt.map_or(0, |rt| rt.ring().generation())
    }

    fn purges_since(&self, generation: u64) -> (u64, Vec<LogPurge>) {
        let Some(rt) = self.rt else {
            return (0, Vec::new());
        };
        let (g, purges) = rt.ring().purges_since(generation);
        let out = purges
            .into_iter()
            .map(|p| LogPurge {
                kinds: p.kinds.kinds().map(data_kind).collect(),
                through: rt.id(p.through),
            })
            .collect();
        (g, out)
    }

    fn purges_covering(&self, seq: u64) -> Vec<LogPurge> {
        let Some(rt) = self.rt else {
            return Vec::new();
        };
        rt.ring()
            .purges_covering(seq)
            .into_iter()
            .map(|p| LogPurge {
                kinds: p.kinds.kinds().map(data_kind).collect(),
                through: rt.id(p.through),
            })
            .collect()
    }

    fn history(&self, query: &HistoryQuery) -> Result<LogHistory, LogsError> {
        let rt = self.rt()?;
        let folder = self.folder().ok();
        super::history::page(rt, folder.as_ref().map(|f| f.dir.as_path()), query)
    }

    fn files(&self) -> LogFiles {
        let state = match self.rt.map(Runtime::file_status) {
            Some(FileStatus::On) => LogFileState::On,
            Some(FileStatus::Stopped) => LogFileState::Stopped,
            Some(FileStatus::Off) | None => LogFileState::Off,
        };
        let folder = match self.folder() {
            Ok(f) => f,
            Err(reason) => {
                return LogFiles {
                    files: Vec::new(),
                    folder: None,
                    index_readable: true,
                    reason: Some(reason.to_string()),
                    state: LogFileState::Unavailable,
                };
            }
        };
        let listed = match files::list(&folder.dir) {
            Ok(l) => l,
            Err(_) => {
                return LogFiles {
                    files: Vec::new(),
                    folder: Some(folder.dir.display().to_string()),
                    index_readable: true,
                    reason: Some("unreadable".into()),
                    state: LogFileState::Unavailable,
                };
            }
        };
        let index = files::read_index(&folder.dir);
        let ours = self.rt.and_then(|rt| rt.opened_folder()) == Some(folder.dir.clone());
        let last_line = self.rt.map_or(0, |rt| rt.last_file_line());
        let mut cache = self.from.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|name, _| listed.iter().any(|l| l.name.file_name() == *name));
        let mut out = Vec::new();
        for l in listed.iter().filter(|l| l.regular) {
            let key = l.name.file_name();
            let from = match cache.get(&key) {
                Some((size, m, from)) if *size == l.size && *m == l.modified => from.clone(),
                _ => {
                    let from = files::first_ts(&folder.dir, &l.name);
                    cache.insert(key.clone(), (l.size, l.modified, from.clone()));
                    from
                }
            };
            let to = match &l.name {
                LogName::Current if ours && last_line > 0 => {
                    Some(Stamp { millis: last_line }.rfc3339())
                }
                LogName::Current => l.modified.and_then(|m| {
                    m.duration_since(std::time::UNIX_EPOCH).ok().map(|d| {
                        Stamp {
                            millis: d.as_millis() as i64,
                        }
                        .rfc3339()
                    })
                }),
                n => n.rotated_at(),
            };
            let flags = index.flags(&l.name);
            out.push(LogFileInfo {
                compressed: l.name.gz(),
                current: l.name == LogName::Current,
                details: flags.details || flags.opt_in,
                from,
                name: key,
                opt_in: flags.opt_in,
                size: l.size,
                to,
            });
        }
        LogFiles {
            files: out,
            folder: Some(folder.dir.display().to_string()),
            index_readable: index.readable,
            reason: None,
            state,
        }
    }

    fn download(
        &self,
        name: &str,
        format: LogFormat,
        redact: Option<bool>,
    ) -> Result<Download, LogsError> {
        let name = LogName::parse(name).ok_or(LogsError::NotFound)?;
        let (folder, listed, index) = self.listing().map_err(|_| LogsError::NotFound)?;
        // It must be in the folder's listing, as a regular file.
        if !listed.iter().any(|l| l.name == name && l.regular) {
            return Err(LogsError::NotFound);
        }
        let flags = index.flags(&name);
        let redact = redact.unwrap_or(flags.details || flags.opt_in);
        let r = open_plain(&folder.dir, &name).map_err(|_| LogsError::NotFound)?;
        Ok(Download {
            file_name: download_name(&name, format, redact),
            content_type: content_type(format),
            chunks: content(r, format, redact),
        })
    }

    fn archive(&self, format: LogFormat, redact: Option<bool>) -> Result<Download, LogsError> {
        let (folder, listed, index) = self.listing()?;
        let listed: Vec<Listed> = listed.into_iter().filter(|l| l.regular).collect();
        let redact = redact.unwrap_or_else(|| {
            !index.readable
                || listed.iter().any(|l| {
                    let f = index.flags(&l.name);
                    f.details || f.opt_in
                })
        });
        let members = listed
            .iter()
            .map(|l| {
                (
                    download_name(&l.name, format, redact),
                    Logs::opener(&folder.dir, &l.name, format, redact),
                )
            })
            .collect();
        let stamp = Stamp::now();
        Ok(Download {
            file_name: format!(
                "lodestar-logs-{}{}.zip",
                stamp.compact(),
                if redact { "-redacted" } else { "" }
            ),
            content_type: "application/zip",
            chunks: Box::new(ZipStream::new(stamp.millis, members)),
        })
    }

    fn delete(&self, which: LogFilesWhich) -> Result<LogFilesDeleted, LogsError> {
        let rt = self.rt()?;
        let (folder, listed, index) = self.listing()?;
        let chosen: Vec<&Listed> = listed
            .iter()
            .filter(|l| match which {
                LogFilesWhich::All => true,
                LogFilesWhich::Details => {
                    let f = index.flags(&l.name);
                    f.details || f.opt_in
                }
            })
            .collect();
        let current = chosen.iter().any(|l| l.name == LogName::Current);
        let names: Vec<LogName> = chosen
            .iter()
            .filter(|l| l.name != LogName::Current)
            .map(|l| l.name.clone())
            .collect();
        let done = rt
            .delete_logs(&folder.dir, names, current, which == LogFilesWhich::All)
            .map_err(LogsError::Internal)?;
        lodestar_log::log_event!(
            events::LOGGING_FILES_DELETED,
            count = done.deleted,
            bytes = done.bytes
        );
        Ok(LogFilesDeleted {
            bytes: done.bytes,
            deleted: done.deleted,
            skipped: done
                .skipped
                .into_iter()
                .map(|(name, reason)| LogFileSkipped {
                    name,
                    reason: reason.to_string(),
                })
                .collect(),
        })
    }
}

fn content_type(format: LogFormat) -> &'static str {
    match format {
        LogFormat::Jsonl => "application/x-ndjson",
        LogFormat::Text => "text/plain; charset=utf-8",
    }
}
