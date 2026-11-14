//! A fake node for tests, behind the `test-support` feature.
//!
//! It answers reads the way ACID's `ReadStream` does: a pin whose line is
//! missing or differs answers `Diverged`; otherwise a page of lines after the
//! cursor, with the log's epoch and length. One fake serves this crate's tests
//! and the CLI's.

use std::cell::{Cell, RefCell};

use acid_protocol::{cursor_from_offset, line_digest, offset_from_cursor, LinePin, PinnedRead};

use crate::ReadFailure;

/// What the node was asked, per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub cursor: Option<String>,
    pub limit: usize,
    pub pin: Option<LinePin>,
}

pub struct FakeNode {
    pub epoch: String,
    pub lines: Vec<String>,
    /// `false`: answer like an old node that sends no epoch.
    pub versioned: bool,
    requests: RefCell<Vec<Request>>,
    fail_at: Cell<Option<usize>>,
}

impl FakeNode {
    /// A log of `count` lines of the form `["track:<i>","x"]`.
    pub fn new(epoch: &str, count: usize) -> Self {
        Self {
            epoch: epoch.to_string(),
            lines: (0..count)
                .map(|i| format!(r#"["track:{i}","x"]"#))
                .collect(),
            versioned: true,
            requests: RefCell::new(Vec::new()),
            fail_at: Cell::new(None),
        }
    }

    /// Fail the `n`th request (0-based, counted from the last
    /// [`FakeNode::forget_requests`]) with a read failure.
    pub fn fail_request(&self, n: usize) {
        self.fail_at.set(Some(n));
    }

    pub fn stop_failing(&self) {
        self.fail_at.set(None);
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.borrow().clone()
    }

    pub fn forget_requests(&self) {
        self.requests.borrow_mut().clear();
    }

    /// Answer one read.
    pub fn serve(
        &self,
        cursor: Option<String>,
        limit: usize,
        pin: Option<LinePin>,
    ) -> Result<PinnedRead, ReadFailure> {
        let index = self.requests.borrow().len();
        self.requests.borrow_mut().push(Request {
            cursor: cursor.clone(),
            limit,
            pin: pin.clone(),
        });
        if self.fail_at.get() == Some(index) {
            return Err(ReadFailure("connection reset".into()));
        }
        if !self.versioned {
            return Ok(PinnedRead::Unversioned);
        }
        if let Some(pin) = pin {
            let holds = self
                .lines
                .get(pin.line)
                .is_some_and(|l| line_digest(l) == pin.sha256);
            if !holds {
                return Ok(PinnedRead::Diverged {
                    epoch: self.epoch.clone(),
                    total_lines: self.lines.len(),
                });
            }
        }
        let from = cursor.as_deref().and_then(offset_from_cursor).unwrap_or(0);
        let page: Vec<String> = self.lines.iter().skip(from).take(limit).cloned().collect();
        Ok(PinnedRead::Chunk {
            cursor: cursor_from_offset(from + page.len()),
            lines: page,
            epoch: self.epoch.clone(),
            total_lines: self.lines.len(),
        })
    }
}
