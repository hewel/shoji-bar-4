//! One coalescing worker owns generation, cache writes and external theme updates.
//! UI publication rejects superseded requests; external writes never overlap.

use super::integrations::{IntegrationManager, IntegrationReport, Paths};
use super::matugen::{self, GeneratedTheme, Request};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Input {
    pub request: Request,
    pub gtk: bool,
    pub zen: bool,
    pub retry: u64,
}

#[derive(Clone, Debug)]
pub(super) struct Outcome {
    pub busy: bool,
    pub theme: Option<Arc<GeneratedTheme>>,
    pub request: Option<Request>,
    pub error: Option<String>,
    pub integrations: IntegrationReport,
}

#[derive(Default)]
struct Mailbox {
    generation: u64,
    pending: Option<Input>,
    stopped: bool,
}

#[derive(Default)]
struct Control {
    mailbox: Mutex<Mailbox>,
    changed: Condvar,
    cancelled: AtomicBool,
}

impl Control {
    fn accepts(&self, generation: u64) -> bool {
        let mailbox = self.mailbox.lock().unwrap();
        !self.cancelled.load(Ordering::Acquire)
            && !mailbox.stopped
            && mailbox.generation == generation
    }
}

struct RequestContext<'a> {
    control: &'a Control,
    generation: u64,
}

impl RequestContext<'_> {
    fn is_current(&self) -> bool {
        self.control.accepts(self.generation)
    }

    /// Staging happens without this lock. Only atomic publication and accepting
    /// the in-memory palette share the selection's linearization point.
    fn commit<T>(&self, commit: impl FnOnce() -> T) -> Option<T> {
        let mailbox = self.control.mailbox.lock().unwrap();
        if self.control.cancelled.load(Ordering::Acquire)
            || mailbox.stopped
            || mailbox.generation != self.generation
        {
            return None;
        }
        Some(commit())
    }
}

pub(super) struct Worker {
    control: Arc<Control>,
}

impl Worker {
    pub fn start(
        paths: Result<Paths, String>,
        cached: Option<Arc<GeneratedTheme>>,
        persistent: bool,
        publish: impl Fn(Outcome) + 'static,
    ) -> Result<Self, String> {
        let control = Arc::new(Control::default());
        let received = Arc::new(Mutex::new(None::<(u64, Outcome)>));
        let pending = received.clone();
        let current = control.clone();
        let wake = anti_webui::prelude::ui_channel(move |()| {
            if let Some((generation, outcome)) = received.lock().unwrap().take()
                && current.accepts(generation)
            {
                publish(outcome);
            }
        });
        let target = control.clone();
        std::thread::Builder::new()
            .name("shoji themes".into())
            .spawn(move || {
                let mut engine = Engine::new(paths, cached, persistent);
                drive(
                    target,
                    |input, current, progress| engine.apply(input, current, progress),
                    move |generation, outcome| {
                        let notify = pending
                            .lock()
                            .unwrap()
                            .replace((generation, outcome))
                            .is_none();
                        if notify {
                            wake.send(());
                        }
                    },
                );
            })
            .map_err(|error| format!("Could not start theme worker: {error}"))?;
        Ok(Self { control })
    }

    pub fn configure(&self, input: Input) {
        let mut mailbox = self.control.mailbox.lock().unwrap();
        mailbox.generation = mailbox.generation.wrapping_add(1);
        mailbox.pending = Some(input);
        self.control.changed.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.control.cancelled.store(true, Ordering::Release);
        let mut mailbox = self.control.mailbox.lock().unwrap();
        mailbox.stopped = true;
        mailbox.pending = None;
        self.control.changed.notify_one();
    }
}

fn drive<T>(
    control: Arc<Control>,
    mut process: impl FnMut(&Input, &RequestContext<'_>, &mut dyn FnMut(T)) -> Option<T>,
    mut publish: impl FnMut(u64, T),
) {
    loop {
        let (generation, input) = {
            let mut mailbox = control.mailbox.lock().unwrap();
            while mailbox.pending.is_none() && !mailbox.stopped {
                mailbox = control.changed.wait(mailbox).unwrap();
            }
            if mailbox.stopped {
                return;
            }
            (mailbox.generation, mailbox.pending.take().unwrap())
        };
        let current = RequestContext {
            control: &control,
            generation,
        };
        let mut progress = |outcome| {
            if current.is_current() {
                publish(generation, outcome);
            }
        };
        if let Some(outcome) = process(&input, &current, &mut progress)
            && current.is_current()
        {
            publish(generation, outcome);
        }
    }
}

struct Engine {
    paths: Result<Paths, String>,
    manager: Option<IntegrationManager>,
    persistent: bool,
    good: Option<Arc<GeneratedTheme>>,
    good_request: Option<Request>,
    attempted: Option<(Request, u64)>,
    error: Option<String>,
}

impl Engine {
    fn new(
        paths: Result<Paths, String>,
        good: Option<Arc<GeneratedTheme>>,
        persistent: bool,
    ) -> Self {
        let manager = if persistent {
            paths
                .as_ref()
                .ok()
                .map(|paths| IntegrationManager::new(paths.clone()))
        } else {
            None
        };
        Self {
            paths,
            manager,
            persistent,
            good,
            good_request: None,
            attempted: None,
            error: None,
        }
    }

    fn apply(
        &mut self,
        input: &Input,
        current: &RequestContext<'_>,
        progress: &mut dyn FnMut(Outcome),
    ) -> Option<Outcome> {
        let attempt = (input.request.clone(), input.retry);
        if self.attempted.as_ref() != Some(&attempt) {
            let generated = matugen::generate(&input.request, &current.control.cancelled);
            if !current.is_current() {
                return None;
            }
            match generated {
                Ok(theme) => {
                    let staged = if self.persistent {
                        self.paths.as_ref().map_err(Clone::clone).and_then(|paths| {
                            matugen::prepare_cache(
                                &paths.state.join("shoji-bar-4/theme/last-good.json"),
                                &theme,
                            )
                            .map(Some)
                        })
                    } else {
                        Ok(None)
                    };
                    self.accept(input, current, theme, staged)?;
                }
                Err(error) => {
                    current.commit(|| {
                        self.attempted = Some(attempt);
                        self.error = Some(format!("{error} Keeping the last available colors."));
                    })?;
                }
            }
        }
        if !current.is_current() {
            return None;
        }
        // Do not make shell repaint wait for filesystem or desktop-settings I/O.
        progress(Outcome {
            busy: true,
            theme: self.good.clone(),
            request: self.good_request.clone(),
            error: self.error.clone(),
            integrations: IntegrationReport {
                gtk: Ok(if input.gtk {
                    "Updating GTK theme files"
                } else {
                    "Checking GTK restoration"
                }
                .into()),
                zen: Ok(if input.zen {
                    "Updating Zen theme files"
                } else {
                    "Checking Zen restoration"
                }
                .into()),
            },
        });
        let integrations = if !self.persistent {
            IntegrationReport {
                gtk: Ok("Preview only — GTK configuration is unchanged".into()),
                zen: Ok("Preview only — Zen configuration is unchanged".into()),
            }
        } else if let Some(manager) = &mut self.manager {
            manager.sync(input.gtk, input.zen, self.good.as_deref())
        } else {
            let error = self.paths.as_ref().unwrap_err().clone();
            IntegrationReport {
                gtk: Err(error.clone()),
                zen: Err(error),
            }
        };
        Some(Outcome {
            busy: false,
            theme: self.good.clone(),
            request: self.good_request.clone(),
            error: self.error.clone(),
            integrations,
        })
    }

    fn accept(
        &mut self,
        input: &Input,
        current: &RequestContext<'_>,
        theme: GeneratedTheme,
        staged: Result<Option<matugen::PreparedCache>, String>,
    ) -> Option<()> {
        let published = current.commit(|| {
            let published = staged
                .and_then(|prepared| prepared.map(matugen::PreparedCache::commit).transpose());
            self.error = published
                .as_ref()
                .err()
                .map(|error| format!("Colors applied, but the cache could not be saved: {error}"));
            self.good = Some(Arc::new(theme));
            self.good_request = Some(input.request.clone());
            self.attempted = Some((input.request.clone(), input.retry));
            published
        })?;
        // fsync may stall on a filesystem. Never let it block the UI mailbox.
        if let Ok(Some(published)) = published
            && let Err(error) = published.sync()
        {
            self.error = Some(format!(
                "Colors applied, but cache durability failed: {error}"
            ));
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::matugen::Source;
    use super::*;
    use crate::settings::Palette;
    use std::sync::mpsc;

    fn input(seed: u32) -> Input {
        Input {
            request: Request {
                source: Source::Color(seed),
                palette: Palette::TonalSpot,
                dark: true,
            },
            gtk: false,
            zen: false,
            retry: 0,
        }
    }

    #[test]
    fn a_late_generation_cannot_publish_and_pending_choices_coalesce() {
        let control = Arc::new(Control::default());
        let worker = Worker {
            control: control.clone(),
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (published_tx, published_rx) = mpsc::channel();
        worker.configure(input(1));
        let thread = std::thread::spawn(move || {
            drive(
                control,
                |input, current, _| {
                    started_tx.send(input.request.source.clone()).unwrap();
                    release_rx.recv().unwrap();
                    current.is_current().then(|| input.request.source.clone())
                },
                |_, outcome| published_tx.send(outcome).unwrap(),
            )
        });
        assert_eq!(started_rx.recv().unwrap(), Source::Color(1));
        worker.configure(input(2));
        worker.configure(input(3));
        release_tx.send(()).unwrap();
        assert_eq!(started_rx.recv().unwrap(), Source::Color(3));
        release_tx.send(()).unwrap();
        assert_eq!(published_rx.recv().unwrap(), Source::Color(3));
        drop(worker);
        thread.join().unwrap();
        assert!(published_rx.try_recv().is_err());
    }

    #[test]
    fn closing_the_owner_discards_in_flight_results() {
        let control = Arc::new(Control::default());
        let worker = Worker {
            control: control.clone(),
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (published_tx, published_rx) = mpsc::channel();
        worker.configure(input(1));
        let thread = std::thread::spawn(move || {
            drive(
                control,
                |_, _, _| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Some(())
                },
                |_, outcome| published_tx.send(outcome).unwrap(),
            )
        });
        started_rx.recv().unwrap();
        drop(worker);
        release_tx.send(()).unwrap();
        thread.join().unwrap();
        assert!(published_rx.recv().is_err());
    }

    #[test]
    fn superseded_staged_palette_cannot_replace_cache_or_failure_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("last-good.json");
        let original = Arc::new(matugen::test_theme());
        matugen::prepare_cache(&path, &original)
            .unwrap()
            .commit()
            .unwrap()
            .sync()
            .unwrap();
        let original_bytes = std::fs::read(&path).unwrap();
        let mut candidate = (*original).clone();
        candidate.source_color = 0x654321;
        candidate
            .colors
            .insert("source_color".into(), candidate.source_color);

        let control = Arc::new(Control::default());
        let worker = Worker {
            control: control.clone(),
        };
        let first = input(candidate.source_color);
        worker.configure(first.clone());
        let stale = RequestContext {
            control: &control,
            generation: 1,
        };
        let staged = matugen::prepare_cache(&path, &candidate).unwrap();
        // Supersede the completed generation while its cache is staged but unpublished.
        let mut next = input(0);
        next.request.source = Source::Image(directory.path().join("missing-wallpaper.png"));
        worker.configure(next.clone());
        let mut engine = Engine::new(
            Err("No external configuration in this test".into()),
            Some(original.clone()),
            false,
        );
        assert!(
            engine
                .accept(&first, &stale, candidate, Ok(Some(staged)))
                .is_none()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
        assert!(Arc::ptr_eq(engine.good.as_ref().unwrap(), &original));

        let current = RequestContext {
            control: &control,
            generation: 2,
        };
        let outcome = engine.apply(&next, &current, &mut |_| {}).unwrap();
        assert!(outcome.error.is_some());
        assert!(Arc::ptr_eq(outcome.theme.as_ref().unwrap(), &original));
        assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
    }

    #[test]
    fn shutdown_discards_a_staged_cache_without_replacing_last_good() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("last-good.json");
        let original = Arc::new(matugen::test_theme());
        matugen::prepare_cache(&path, &original)
            .unwrap()
            .commit()
            .unwrap()
            .sync()
            .unwrap();
        let original_bytes = std::fs::read(&path).unwrap();
        let mut candidate = (*original).clone();
        candidate.source_color = 0x654321;
        candidate
            .colors
            .insert("source_color".into(), candidate.source_color);
        let control = Arc::new(Control::default());
        let worker = Worker {
            control: control.clone(),
        };
        let requested = input(candidate.source_color);
        worker.configure(requested.clone());
        let staged = matugen::prepare_cache(&path, &candidate).unwrap();
        let current = RequestContext {
            control: &control,
            generation: 1,
        };
        let mut engine = Engine::new(
            Err("No external configuration in this test".into()),
            Some(original.clone()),
            false,
        );
        drop(worker);
        assert!(
            engine
                .accept(&requested, &current, candidate, Ok(Some(staged)))
                .is_none()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
        assert!(Arc::ptr_eq(engine.good.as_ref().unwrap(), &original));
    }
}
