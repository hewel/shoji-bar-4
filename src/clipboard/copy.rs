//! Copy feedback belongs to a logical panel invocation, not its animated view lifetime.

use crate::clipboard_service;
use crate::panels::{self, Invocation, Panel};
use anti_webui::prelude::*;
use anti_webui::shell::prelude::Output;

#[derive(Default)]
struct Feedback {
    invocation: Option<Invocation>,
    pending: Option<u64>,
    error: String,
}

#[derive(Clone, Copy)]
pub(crate) struct Copy {
    invocation: Memo<Option<Invocation>>,
    feedback: Signal<Feedback>,
}

impl Copy {
    pub(crate) fn new(output: &Output) -> Self {
        let invocation = panels::invocation(output, Panel::Clipboard);
        let feedback = signal(Feedback::default());
        // Show can reuse its exiting owner. Logical dismissal must therefore
        // invalidate feedback before disposal, including batched close/reopen.
        effect(move || {
            let current = invocation.get();
            if feedback.with_untracked(|f| f.invocation != current) {
                feedback.set(Feedback {
                    invocation: current,
                    ..Default::default()
                });
            }
        });
        let state = clipboard_service::state();
        effect(move || {
            let current = invocation.get();
            let waiting = feedback.with(|f| f.invocation.clone().zip(f.pending));
            let result = state.with_copy_results(|results| {
                waiting.as_ref().and_then(|(_, request)| {
                    results
                        .iter()
                        .find(|(id, _)| id == request)
                        .map(|(_, result)| result.clone())
                })
            });
            let Some(((opening, _), result)) = waiting.zip(result) else {
                return;
            };
            feedback.update(|f| f.pending = None);
            if current.as_ref() != Some(&opening) {
                return;
            }
            match result {
                Ok(()) => panels::close_invocation(&opening),
                Err(message) => feedback.update(|f| f.error = message),
            }
        });
        Self {
            invocation,
            feedback,
        }
    }

    pub(crate) fn choose(self, id: u64) {
        let Some(invocation) = untrack(|| self.invocation.get()) else {
            return;
        };
        // Establish the association here too: choosing can happen before a
        // batched navigation change has flushed its invalidation effect.
        self.feedback.set(Feedback {
            invocation: Some(invocation),
            ..Default::default()
        });
        match clipboard_service::copy(id) {
            Ok(request) => self.feedback.update(|f| f.pending = Some(request)),
            Err(error) => self.feedback.update(|f| f.error = error),
        }
    }

    pub(crate) fn remove(self, id: u64) {
        self.edit(|| clipboard_service::remove(id));
    }

    pub(crate) fn toggle_pin(self, id: u64) {
        self.edit(|| clipboard_service::toggle_pin(id));
    }

    fn edit(self, action: impl FnOnce() -> Result<(), String>) {
        if let Some(invocation) = untrack(|| self.invocation.get()) {
            self.feedback.update(|f| {
                if f.invocation.as_ref() != Some(&invocation) {
                    *f = Feedback {
                        invocation: Some(invocation),
                        ..Default::default()
                    };
                }
                f.error = action().err().unwrap_or_default();
            });
        }
    }

    pub(crate) fn error(self) -> String {
        self.feedback.with(|f| f.error.clone())
    }
}
