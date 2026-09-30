//! The document loop's state machine (`docs/audits/shared-core.md`, step
//! 5): run once after a pause in typing, supersede older runs, keep the
//! session's buffer in step with the host's text, run the last mode again
//! when a file the model read changes, and keep the customizer's edited
//! values (dropping those of parameters that vanished).
//!
//! The macOS app kept this in Swift (`DocumentLoop.swift`) and the web demo
//! again in JavaScript; the Linux and Windows apps take it from here. It
//! has no clock and no thread (this is a library crate, `CLAUDE.md`): the
//! host passes "now" in milliseconds on any monotonic clock of its own,
//! keeps one timer armed for [`DocumentLoop::next_due_ms`], and when it
//! fires asks [`DocumentLoop::due`] what to run. Every transition is
//! therefore a plain function of the calls made, and the unit tests drive
//! it with made-up times.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{DocumentRequest, ParameterGroup, ParameterOverride, ParameterValue, RenderMode};

/// A run the host is to start now ([`DocumentLoop::begin_run`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPlan {
    /// Which run this is; [`DocumentLoop::is_current`] says whether a
    /// result is still wanted.
    pub generation: u64,
    /// The path the session knows the document by.
    pub path: String,
    /// A path to close first: the document was saved under a new name,
    /// and the old buffer would shadow the file still at the old path.
    pub close: Option<String>,
    /// Whether the session's buffer is behind the host's text: send it
    /// whole (`update`), then call [`DocumentLoop::text_sent`].
    pub send_text: bool,
    pub request: DocumentRequest,
}

/// The loop's state as a host shows it (the observer's record).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentState {
    pub path: Option<String>,
    pub in_sync: bool,
    /// Runs started so far.
    pub request_count: u64,
    pub last_mode: Option<RenderMode>,
    /// The run waiting for its moment, and when (the host's clock).
    pub pending_mode: Option<RenderMode>,
    pub due_ms: Option<u64>,
    /// The customizer's edited values, by name.
    pub overrides: Vec<ParameterOverride>,
    pub selected_set: Option<String>,
    pub parts: bool,
    pub closed: bool,
}

/// One document's loop.
#[derive(Debug, Clone)]
pub struct DocumentLoop {
    delay_ms: u64,
    path: Option<String>,
    in_sync: bool,
    pending: Option<(RenderMode, u64)>,
    last_mode: Option<RenderMode>,
    request_count: u64,
    generation: u64,
    values: BTreeMap<String, ParameterValue>,
    selected_set: Option<String>,
    parts: bool,
    closed: bool,
}

impl DocumentLoop {
    /// A loop that runs `delay_ms` after the last edit
    /// ([`crate::DEFAULT_PREVIEW_DELAY_MS`] unless the host has a reason).
    pub fn new(delay_ms: u64) -> DocumentLoop {
        DocumentLoop {
            delay_ms,
            path: None,
            in_sync: false,
            pending: None,
            last_mode: None,
            request_count: 0,
            generation: 0,
            values: BTreeMap::new(),
            selected_set: None,
            parts: false,
            closed: false,
        }
    }

    pub fn delay_ms(&self) -> u64 {
        self.delay_ms
    }

    pub fn state(&self) -> DocumentState {
        DocumentState {
            path: self.path.clone(),
            in_sync: self.in_sync,
            request_count: self.request_count,
            last_mode: self.last_mode,
            pending_mode: self.pending.map(|p| p.0),
            due_ms: self.pending.map(|p| p.1),
            overrides: self.overrides(),
            selected_set: self.selected_set.clone(),
            parts: self.parts,
            closed: self.closed,
        }
    }

    // --- Timing -------------------------------------------------------------

    /// A preview once typing pauses: each call restarts the wait.
    pub fn schedule(&mut self, now_ms: u64) {
        if !self.closed {
            self.pending = Some((RenderMode::Preview, now_ms.saturating_add(self.delay_ms)));
        }
    }

    /// When the host's timer should fire next, if anything waits.
    pub fn next_due_ms(&self) -> Option<u64> {
        self.pending.map(|p| p.1)
    }

    /// The run whose moment has come, taken off the queue; `None` when
    /// nothing waits or it is not yet time (re-arm for `next_due_ms`).
    pub fn due(&mut self, now_ms: u64) -> Option<RenderMode> {
        match self.pending {
            Some((mode, at)) if now_ms >= at && !self.closed => {
                self.pending = None;
                Some(mode)
            }
            _ => None,
        }
    }

    // --- Runs ---------------------------------------------------------------

    /// Start a run of `mode` on the document at `path` now: whatever waited
    /// is dropped (this run covers it) and older runs are superseded.
    pub fn begin_run(&mut self, mode: RenderMode, path: &str) -> Option<RunPlan> {
        if self.closed {
            return None;
        }
        self.pending = None;
        let close = match &self.path {
            Some(old) if old != path => {
                self.in_sync = false;
                Some(old.clone())
            }
            _ => None,
        };
        self.path = Some(path.to_string());
        self.request_count += 1;
        self.generation += 1;
        self.last_mode = Some(mode);
        Some(RunPlan {
            generation: self.generation,
            path: path.to_string(),
            close,
            send_text: !self.in_sync,
            request: DocumentRequest {
                mode,
                overrides: self.overrides(),
                parts: self.parts,
                enable: Vec::new(),
            },
        })
    }

    /// The document is known to the session as `path` from now on, without
    /// a run (a panel's request before any run). The old path to close
    /// when it changed; the text must then be sent again.
    pub fn set_path(&mut self, path: &str) -> Option<String> {
        let old = self.path.replace(path.to_string());
        match old {
            Some(o) if o != path => {
                self.in_sync = false;
                Some(o)
            }
            Some(_) => None,
            None => {
                self.in_sync = false;
                None
            }
        }
    }

    /// Whether run `generation` is the latest (an older one's result is
    /// not shown).
    pub fn is_current(&self, generation: u64) -> bool {
        !self.closed && generation == self.generation
    }

    /// The session's buffer now holds the host's text.
    pub fn text_sent(&mut self) {
        self.in_sync = true;
    }

    /// The host's text changed in a way the session did not see (a file
    /// read, the editor's whole text after the copies disagreed, a failed
    /// or mismatched edit): the next run sends it whole.
    pub fn text_replaced(&mut self) {
        self.in_sync = false;
    }

    pub fn in_sync(&self) -> bool {
        self.in_sync
    }

    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    pub fn request_count(&self) -> u64 {
        self.request_count
    }

    pub fn last_mode(&self) -> Option<RenderMode> {
        self.last_mode
    }

    /// A file the last run read changed on disk: a render runs again at
    /// once (`Some`), anything else after the usual pause.
    pub fn files_changed(&mut self, now_ms: u64) -> Option<RenderMode> {
        if self.closed {
            return None;
        }
        if self.last_mode == Some(RenderMode::Render) {
            Some(RenderMode::Render)
        } else {
            self.schedule(now_ms);
            None
        }
    }

    /// Nothing more runs.
    pub fn close(&mut self) {
        self.closed = true;
        self.pending = None;
    }

    // --- The customizer -----------------------------------------------------

    /// The edited values as a run takes them, sorted by name so equal
    /// values give equal requests (and equal cache keys).
    pub fn overrides(&self) -> Vec<ParameterOverride> {
        self.values
            .iter()
            .map(|(name, value)| ParameterOverride {
                name: name.clone(),
                value: value.clone(),
            })
            .collect()
    }

    /// Set (or with `None`, drop) one edited value. Whether anything
    /// changed; a change leaves any selected set and schedules a run.
    pub fn set_parameter(
        &mut self,
        name: &str,
        value: Option<ParameterValue>,
        now_ms: u64,
    ) -> bool {
        let changed = match value {
            Some(v) => {
                self.values.get(name) != Some(&v) && {
                    self.values.insert(name.to_string(), v);
                    true
                }
            }
            None => self.values.remove(name).is_some(),
        };
        if changed {
            self.selected_set = None;
            self.schedule(now_ms);
        }
        changed
    }

    /// Every parameter back to the text's value.
    pub fn reset_parameters(&mut self, now_ms: u64) -> bool {
        self.selected_set = None;
        if self.values.is_empty() {
            return false;
        }
        self.values.clear();
        self.schedule(now_ms);
        true
    }

    /// A parameter set was applied (`values` from the core's
    /// `apply_parameter_set`, checked and clamped): those equal to the
    /// text's own are no override.
    pub fn parameter_set_applied(
        &mut self,
        name: &str,
        values: Vec<ParameterOverride>,
        groups: &[ParameterGroup],
        now_ms: u64,
    ) {
        let default = |n: &str| {
            groups
                .iter()
                .flat_map(|g| &g.parameters)
                .find(|p| p.name == n)
                .map(|p| &p.default_value)
        };
        self.values = values
            .into_iter()
            .filter(|o| default(&o.name) != Some(&o.value))
            .map(|o| (o.name, o.value))
            .collect();
        self.selected_set = Some(name.to_string());
        self.schedule(now_ms);
    }

    /// The parameters of the latest text: edited values of parameters that
    /// are gone are dropped. Whether any were.
    pub fn parameters_read(&mut self, groups: &[ParameterGroup]) -> bool {
        let before = self.values.len();
        self.values.retain(|name, _| {
            groups
                .iter()
                .any(|g| g.parameters.iter().any(|p| &p.name == name))
        });
        self.values.len() != before
    }

    /// neoscad's `part()` extension on or off. Whether it changed.
    pub fn set_parts(&mut self, on: bool) -> bool {
        let changed = self.parts != on;
        self.parts = on;
        changed
    }

    pub fn parts(&self) -> bool {
        self.parts
    }
}
