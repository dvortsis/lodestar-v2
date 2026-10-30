//! The outcome log: every harvest, fetch and store, stamped with virtual
//! milliseconds. The golden keeps only its length and fingerprint;
//! `CRAWLER_SIM_LOG=<dir>` writes it out in full for diffing two runs.

use std::fmt::{Arguments, Write as _};

use tokio::time::Instant;

use crate::rng::Fnv;

#[derive(Debug)]
pub struct Log {
    start: Instant,
    pub events: u64,
    pub fnv: Fnv,
    lines: Option<String>,
}

impl Log {
    pub fn new(start: Instant, keep: bool) -> Log {
        Log {
            start,
            events: 0,
            fnv: Fnv::default(),
            lines: keep.then(String::new),
        }
    }

    pub fn event(&mut self, what: Arguments<'_>) {
        let ms = Instant::now().duration_since(self.start).as_millis();
        let line = format!("{ms:>9} {what}\n");
        self.events += 1;
        self.fnv.write(line.as_bytes());
        if let Some(lines) = &mut self.lines {
            let _ = lines.write_str(&line);
        }
    }

    pub fn take_lines(&mut self) -> Option<String> {
        self.lines.take()
    }
}
