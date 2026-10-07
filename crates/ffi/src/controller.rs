//! A document window's loop ([`client::DocumentLoop`]) for the hosts that
//! take the core through UniFFI: `DocumentController`, with a
//! `DocumentObserver` told the state after every change.
//!
//! The host keeps one timer and its text. It passes "now" (milliseconds on
//! any monotonic clock of its own) to the calls that schedule, arms its
//! timer for `next_due_ms`, and when it fires runs what `due` returns: it
//! calls `begin_run`, closes and sends what the plan says, runs
//! `Core::run_document` with the plan's request, and shows the result only
//! while `is_current` holds. All calls are synchronous and cheap (no
//! evaluation happens here), so a host calls them on its UI thread; the
//! observer is called on the calling thread before the call returns.

use std::sync::{Arc, Mutex, PoisonError};

pub use client::{DocumentState, RunPlan};

use crate::{
    CoreError, DocumentRequest, ParameterGroup, ParameterOverride, ParameterValue, RenderMode,
    guarded,
};

/// A run the host is to start now.
#[uniffi::remote(Record)]
pub struct RunPlan {
    pub generation: u64,
    pub path: String,
    pub close: Option<String>,
    pub send_text: bool,
    pub request: DocumentRequest,
}

/// The loop's state as a host shows it.
#[uniffi::remote(Record)]
pub struct DocumentState {
    pub path: Option<String>,
    pub in_sync: bool,
    pub request_count: u64,
    pub last_mode: Option<RenderMode>,
    pub pending_mode: Option<RenderMode>,
    pub due_ms: Option<u64>,
    pub overrides: Vec<ParameterOverride>,
    pub selected_set: Option<String>,
    pub parts: bool,
    pub closed: bool,
}

/// Told the loop's state after each change (edited values, a run started,
/// the parts toggle), on the thread that made the change.
#[uniffi::export(with_foreign)]
pub trait DocumentObserver: Send + Sync {
    fn state_changed(&self, state: DocumentState);
}

/// One document's loop.
#[derive(uniffi::Object)]
pub struct DocumentController {
    inner: Mutex<client::DocumentLoop>,
    observer: Mutex<Option<Arc<dyn DocumentObserver>>>,
}

impl std::fmt::Debug for DocumentController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentController").finish_non_exhaustive()
    }
}

impl DocumentController {
    /// Run `f` on the loop, then tell the observer when `f` says something
    /// changed. The observer is called after the lock is released, so it
    /// may call back into the controller.
    fn change<T>(
        &self,
        f: impl FnOnce(&mut client::DocumentLoop) -> (T, bool),
    ) -> Result<T, CoreError> {
        guarded(|| {
            let (out, state) = {
                let mut l = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
                let (out, changed) = f(&mut l);
                (out, changed.then(|| l.state()))
            };
            if let Some(state) = state {
                let observer = self
                    .observer
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                if let Some(o) = observer {
                    o.state_changed(state);
                }
            }
            Ok(out)
        })
    }

    fn read<T>(&self, f: impl FnOnce(&client::DocumentLoop) -> T) -> Result<T, CoreError> {
        guarded(|| {
            Ok(f(&self
                .inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)))
        })
    }
}

#[uniffi::export]
impl DocumentController {
    /// A loop that runs `delay_ms` after the last edit; `None` takes the
    /// core's default (`default_preview_delay_ms`).
    #[uniffi::constructor]
    pub fn new(delay_ms: Option<u64>) -> Arc<DocumentController> {
        Arc::new(DocumentController {
            inner: Mutex::new(client::DocumentLoop::new(
                delay_ms.unwrap_or(client::DEFAULT_PREVIEW_DELAY_MS),
            )),
            observer: Mutex::new(None),
        })
    }

    /// Tell `observer` about every later change (replacing any before).
    pub fn set_observer(
        &self,
        observer: Option<Arc<dyn DocumentObserver>>,
    ) -> Result<(), CoreError> {
        guarded(|| {
            *self.observer.lock().unwrap_or_else(PoisonError::into_inner) = observer;
            Ok(())
        })
    }

    pub fn state(&self) -> Result<DocumentState, CoreError> {
        self.read(client::DocumentLoop::state)
    }

    pub fn preview_delay_ms(&self) -> Result<u64, CoreError> {
        self.read(client::DocumentLoop::delay_ms)
    }

    /// A preview once typing pauses (each call restarts the wait).
    pub fn schedule(&self, now_ms: u64) -> Result<(), CoreError> {
        self.change(|l| (l.schedule(now_ms), true))
    }

    /// When the host's timer should fire next.
    pub fn next_due_ms(&self) -> Result<Option<u64>, CoreError> {
        self.read(client::DocumentLoop::next_due_ms)
    }

    /// The run whose moment has come (taken off the queue), if any.
    pub fn due(&self, now_ms: u64) -> Result<Option<RenderMode>, CoreError> {
        self.change(|l| {
            let m = l.due(now_ms);
            (m, m.is_some())
        })
    }

    /// Start a run of `mode` on `path` now; `None` once closed.
    pub fn begin_run(&self, mode: RenderMode, path: String) -> Result<Option<RunPlan>, CoreError> {
        self.change(|l| {
            let p = l.begin_run(mode, &path);
            let changed = p.is_some();
            (p, changed)
        })
    }

    /// Known to the session as `path` from now on; the old path to close.
    pub fn set_path(&self, path: String) -> Result<Option<String>, CoreError> {
        self.change(|l| (l.set_path(&path), true))
    }

    /// Whether run `generation` is still the latest.
    pub fn is_current(&self, generation: u64) -> Result<bool, CoreError> {
        self.read(|l| l.is_current(generation))
    }

    /// The session's buffer now holds the host's text.
    pub fn text_sent(&self) -> Result<(), CoreError> {
        self.change(|l| (l.text_sent(), true))
    }

    /// The host's text changed without the session seeing it; the next
    /// run sends it whole.
    pub fn text_replaced(&self) -> Result<(), CoreError> {
        self.change(|l| (l.text_replaced(), true))
    }

    /// A file the last run read changed: `Some(mode)` to run now, else a
    /// preview was scheduled.
    pub fn files_changed(&self, now_ms: u64) -> Result<Option<RenderMode>, CoreError> {
        self.change(|l| (l.files_changed(now_ms), true))
    }

    pub fn set_parameter(
        &self,
        name: String,
        value: Option<ParameterValue>,
        now_ms: u64,
    ) -> Result<bool, CoreError> {
        self.change(|l| {
            let c = l.set_parameter(&name, value, now_ms);
            (c, c)
        })
    }

    pub fn reset_parameters(&self, now_ms: u64) -> Result<bool, CoreError> {
        self.change(|l| (l.reset_parameters(now_ms), true))
    }

    /// A parameter set was applied (`values` from `apply_parameter_set`).
    pub fn parameter_set_applied(
        &self,
        name: String,
        values: Vec<ParameterOverride>,
        groups: Vec<ParameterGroup>,
        now_ms: u64,
    ) -> Result<(), CoreError> {
        self.change(|l| {
            (
                l.parameter_set_applied(&name, values, &groups, now_ms),
                true,
            )
        })
    }

    /// The latest text's parameters: values of vanished ones are dropped.
    pub fn parameters_read(&self, groups: Vec<ParameterGroup>) -> Result<bool, CoreError> {
        self.change(|l| {
            let c = l.parameters_read(&groups);
            (c, c)
        })
    }

    pub fn set_parts(&self, on: bool) -> Result<bool, CoreError> {
        self.change(|l| {
            let c = l.set_parts(on);
            (c, c)
        })
    }

    /// The `--enable` names the runs pass from now on (the app's setting
    /// for constrained sketches, `sketch`). Whether they changed.
    pub fn set_enable(&self, enable: Vec<String>) -> Result<bool, CoreError> {
        self.change(|l| {
            let c = l.set_enable(&enable);
            (c, c)
        })
    }

    /// The edited values as a run takes them, sorted by name.
    pub fn overrides(&self) -> Result<Vec<ParameterOverride>, CoreError> {
        self.read(client::DocumentLoop::overrides)
    }

    /// Nothing more runs.
    pub fn close(&self) -> Result<(), CoreError> {
        self.change(|l| (l.close(), true))
    }
}
