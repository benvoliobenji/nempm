//! Progress events and cancellation for long operations.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use ts_rs::TS;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Stage {
    Queued,
    Download,
    Verify,
    Copy,
    Readback,
    Extract,
    Done,
    Error,
}

/// Emitted as the `progress` event: `{ id, stage, bytesDone, bytesTotal }`.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Progress {
    pub id: String,
    pub stage: Stage,
    #[ts(type = "number")]
    pub bytes_done: u64,
    #[ts(type = "number")]
    pub bytes_total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
}

pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

pub fn discard() -> ProgressFn {
    Arc::new(|_| {})
}

const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Reports one package's progress, throttling byte updates to about ten a
/// second so a fast copy doesn't flood the UI.
pub struct Reporter {
    sink: ProgressFn,
    id: String,
    last: Mutex<Option<(Stage, Instant)>>,
}

impl Reporter {
    pub fn new(sink: ProgressFn, id: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            sink,
            id: id.into(),
            last: Mutex::new(None),
        })
    }

    fn send(&self, stage: Stage, done: u64, total: u64, message: Option<String>) {
        *self.last.lock().unwrap() = Some((stage, Instant::now()));
        (self.sink)(Progress {
            id: self.id.clone(),
            stage,
            bytes_done: done,
            bytes_total: total,
            message,
        });
    }

    /// Always emitted: a stage starts.
    pub fn stage(&self, stage: Stage, done: u64, total: u64) {
        self.send(stage, done, total, None);
    }

    /// Emitted at most every 100 ms, plus on stage change and completion.
    pub fn bytes(&self, stage: Stage, done: u64, total: u64) {
        let due = match *self.last.lock().unwrap() {
            Some((last_stage, at)) => last_stage != stage || at.elapsed() >= MIN_INTERVAL || done >= total,
            None => true,
        };
        if due {
            self.send(stage, done, total, None);
        }
    }

    pub fn done(&self, total: u64, message: Option<String>) {
        self.send(Stage::Done, total, total, message);
    }

    pub fn error(&self, message: String) {
        self.send(Stage::Error, 0, 0, Some(message));
    }
}

/// Cooperative cancellation, checked between chunks.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_updates_are_throttled_but_stage_changes_are_not() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let reporter = Reporter::new(Arc::new(move |p| sink.lock().unwrap().push(p)), "a");
        reporter.stage(Stage::Download, 0, 100);
        for i in 1..50 {
            reporter.bytes(Stage::Download, i, 100);
        }
        reporter.bytes(Stage::Download, 100, 100);
        reporter.bytes(Stage::Verify, 0, 100);
        let events = events.lock().unwrap();
        assert!(events.len() <= 4, "got {} events", events.len());
        assert_eq!(events.last().unwrap().stage, Stage::Verify);
        assert!(events.iter().any(|e| e.bytes_done == 100 && e.stage == Stage::Download));
    }

    #[test]
    fn cancel_token() {
        let token = CancelToken::new();
        assert!(token.check().is_ok());
        token.clone().cancel();
        assert!(matches!(token.check(), Err(Error::Cancelled)));
        token.reset();
        assert!(token.check().is_ok());
    }
}
