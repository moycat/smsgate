//! Fixed-size SMS history ring buffer (heapless::Deque<LogEntry, 50>).

use heapless::Deque;

const CAPACITY: usize = 50;

/// A single entry in the SMS history log.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub sender: String,
    pub body_preview: String, // first 80 chars
    pub timestamp: String,
    pub forwarded: bool,
}

/// Ring buffer of recent SMS history.
pub struct LogRing {
    entries: Deque<LogEntry, CAPACITY>,
}

impl LogRing {
    pub fn new() -> Self {
        LogRing {
            entries: Deque::new(),
        }
    }

    /// Push a new entry, evicting the oldest if full.
    pub fn push(&mut self, entry: LogEntry) {
        if self.entries.is_full() {
            self.entries.pop_front();
        }
        // Safe: we just made room
        let _ = self.entries.push_back(entry);
    }

    /// Return the last `n` entries (most-recent last).
    pub fn last_n(&self, n: usize) -> Vec<&LogEntry> {
        let total = self.entries.len();
        let skip = total.saturating_sub(n);
        self.entries.iter().skip(skip).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for LogRing {
    fn default() -> Self {
        Self::new()
    }
}
