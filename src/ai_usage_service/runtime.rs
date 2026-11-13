use super::{Error, Job, now};
use anti_webui::prelude::*;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Scheduling uses monotonic seconds; only published dates use wall time.
/// One pending bit coalesces refresh bursts, including requests during I/O.
pub(super) struct Schedule<T> {
    pub snapshot: Job<T>,
    interval: u64,
    gap: u64,
    last_start: Option<u64>,
    last_success: Option<u64>,
    next: u64,
    blocked_until: u64,
    failures: u32,
    pending: bool,
}

impl<T> Schedule<T> {
    pub fn new(interval: u64, gap: u64) -> Self {
        Self {
            snapshot: Job::default(),
            interval,
            gap,
            last_start: None,
            last_success: None,
            next: 0,
            blocked_until: 0,
            failures: 0,
            pending: false,
        }
    }

    pub fn configure(&mut self, interval: u64) -> bool {
        let changed = self.interval != interval;
        if changed {
            self.interval = interval;
            if self.snapshot.error.is_none()
                && let Some(at) = self.last_success
            {
                self.next = at.saturating_add(interval);
            }
        }
        changed
    }

    pub fn request(&mut self) {
        self.pending = true;
    }

    pub fn deadline(&self) -> u64 {
        let target = if self.pending { 0 } else { self.next };
        target
            .max(self.blocked_until)
            .max(self.last_start.map_or(0, |at| at.saturating_add(self.gap)))
    }

    pub fn start(&mut self, tick: u64, utc: i64) -> bool {
        if self.snapshot.busy || tick < self.deadline() {
            return false;
        }
        self.pending = false;
        self.last_start = Some(tick);
        self.snapshot.busy = true;
        self.publish_deadline(tick, utc);
        true
    }

    pub fn finish(&mut self, tick: u64, utc: i64, result: Result<T, Error>) {
        self.snapshot.busy = false;
        match result {
            Ok(data) => {
                self.snapshot.data = Some(Arc::new(data));
                self.snapshot.updated_at = Some(utc);
                self.snapshot.error = None;
                self.failures = 0;
                self.blocked_until = 0;
                self.last_success = Some(tick);
                self.next = tick.saturating_add(self.interval);
            }
            Err(error) => {
                self.snapshot.error = Some(error);
                self.failures = self.failures.saturating_add(1);
                let delay = self
                    .interval
                    .saturating_mul(1 << (self.failures - 1).min(5))
                    .min(3600);
                let delay = if error == Error::RateLimit {
                    delay.max(900)
                } else {
                    delay
                };
                self.next = tick.saturating_add(delay);
                self.blocked_until = self.next;
                // Manual clicks cannot turn a failed attempt into a retry burst.
                self.pending = false;
            }
        }
        self.publish_deadline(tick, utc);
    }

    fn publish_deadline(&mut self, tick: u64, utc: i64) {
        self.snapshot.next_at = Some(
            utc.saturating_add(self.deadline().saturating_sub(tick).min(i64::MAX as u64) as i64),
        );
    }
}

struct Control {
    inner: Mutex<Requested>,
    changed: Condvar,
}

struct Requested {
    enabled: bool,
    interval: u64,
    generation: u64,
    refresh: bool,
    stopped: bool,
}

impl Requested {
    fn accepts(&self, generation: u64) -> bool {
        self.enabled && !self.stopped && self.generation == generation
    }
}

pub(super) struct Worker {
    control: Arc<Control>,
}

impl Worker {
    pub fn start<T: Clone + Send + Sync + 'static>(
        name: &str,
        output: Signal<Job<T>>,
        interval: u64,
        gap: u64,
        load: fn() -> Result<T, Error>,
    ) -> Self {
        let control = Arc::new(Control {
            inner: Mutex::new(Requested {
                enabled: false,
                interval,
                generation: 0,
                refresh: false,
                stopped: false,
            }),
            changed: Condvar::new(),
        });
        let pending = Arc::new(Mutex::new(None::<(u64, Job<T>)>));
        let received = pending.clone();
        let current = control.clone();
        let wake = ui_channel(move |()| {
            let update = received.lock().unwrap().take();
            if let Some((generation, snapshot)) = update
                && current.inner.lock().unwrap().accepts(generation)
            {
                output.set(snapshot);
            }
        });
        let target = control.clone();
        let started = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                run(target, interval, gap, load, move |generation, snapshot| {
                    // At most one UI wake and one snapshot per job can be queued.
                    let notify = pending
                        .lock()
                        .unwrap()
                        .replace((generation, snapshot))
                        .is_none();
                    if notify {
                        wake.send(());
                    }
                });
            });
        if started.is_err() {
            output.set(Job {
                error: Some(Error::Start),
                ..Job::default()
            });
        }
        Self { control }
    }

    /// Returns true when disabling invalidates the visible snapshot immediately.
    pub fn configure(&self, enabled: bool, interval: u64) -> bool {
        let mut request = self.control.inner.lock().unwrap();
        let changed = request.enabled != enabled;
        if changed {
            request.enabled = enabled;
            request.generation = request.generation.wrapping_add(1);
            request.refresh = false;
        }
        request.interval = interval;
        self.control.changed.notify_one();
        changed && !enabled
    }

    pub fn refresh(&self) {
        let mut request = self.control.inner.lock().unwrap();
        if request.enabled && !request.stopped {
            request.refresh = true;
            self.control.changed.notify_one();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let mut request = self.control.inner.lock().unwrap();
        request.stopped = true;
        request.generation = request.generation.wrapping_add(1);
        self.control.changed.notify_one();
        // Do not block the UI waiting for network/CLI shutdown. The bounded
        // adapter finishes independently; stopped generations never publish.
    }
}

fn run<T: Clone>(
    control: Arc<Control>,
    interval: u64,
    gap: u64,
    load: impl Fn() -> Result<T, Error>,
    publish: impl Fn(u64, Job<T>),
) {
    let clock = Instant::now();
    let mut generation = 0;
    let mut schedule = Schedule::new(interval, gap);
    loop {
        let mut request = control.inner.lock().unwrap();
        if request.stopped {
            return;
        }
        if generation != request.generation {
            generation = request.generation;
            schedule = Schedule::new(request.interval, gap);
        }
        let configured = schedule.configure(request.interval);
        if !request.enabled {
            drop(control.changed.wait(request).unwrap());
            continue;
        }
        let refreshed = std::mem::take(&mut request.refresh);
        if refreshed {
            schedule.request();
        }
        let tick = clock.elapsed().as_secs();
        if !schedule.start(tick, now()) {
            if configured || refreshed {
                schedule.publish_deadline(tick, now());
                publish(generation, schedule.snapshot.clone());
            }
            let wait = Duration::from_secs(schedule.deadline().saturating_sub(tick).max(1));
            drop(control.changed.wait_timeout(request, wait).unwrap());
            continue;
        }
        publish(generation, schedule.snapshot.clone());
        drop(request);
        let result = load();
        let mut request = control.inner.lock().unwrap();
        if !request.accepts(generation) {
            continue;
        }
        schedule.configure(request.interval);
        if std::mem::take(&mut request.refresh) {
            schedule.request();
        }
        schedule.finish(clock.elapsed().as_secs(), now(), result);
        publish(generation, schedule.snapshot.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_bursts_are_singleflight_and_keep_only_one_followup() {
        let mut job = Schedule::new(300, 5);
        assert!(job.start(0, 1000));
        for _ in 0..100 {
            job.request();
        }
        assert!(!job.start(1, 1001));
        job.finish(1, 1001, Ok("first"));
        assert_eq!(job.snapshot.next_at, Some(1005));
        assert!(!job.start(4, 1004));
        assert!(job.start(5, 1005));
        job.finish(6, 1006, Ok("second"));
        assert!(!job.start(10, 1010));
        assert!(job.start(306, 1306));
    }

    #[test]
    fn failed_refresh_retains_data_and_manual_clicks_cannot_bypass_backoff() {
        let mut job = Schedule::new(600, 60);
        assert!(job.start(0, 1000));
        job.finish(1, 1001, Ok("cached"));
        let cached = job.snapshot.data.clone().unwrap();
        assert!(job.start(601, 1601));
        job.finish(602, 1602, Err(Error::RateLimit));
        assert!(Arc::ptr_eq(&cached, job.snapshot.data.as_ref().unwrap()));
        assert_eq!(job.snapshot.updated_at, Some(1001));
        assert!(!job.snapshot.busy);
        assert_eq!(job.snapshot.error, Some(Error::RateLimit));
        for _ in 0..100 {
            job.request();
        }
        assert!(!job.start(1501, 2501));
        assert!(job.start(1502, 2502));
        job.finish(1503, 2503, Err(Error::Timeout));
        assert_eq!(job.snapshot.next_at, Some(3703));
    }

    #[test]
    fn jobs_make_independent_progress_and_backoff_is_capped() {
        let mut usage = Schedule::<()>::new(60, 5);
        let mut public = Schedule::new(300, 60);
        assert!(usage.start(0, 0));
        assert!(public.start(0, 0));
        public.finish(1, 1, Ok("public"));
        assert!(usage.snapshot.busy);
        assert_eq!(public.snapshot.data.as_deref(), Some(&"public"));
        let mut tick = 1;
        for _ in 0..20 {
            tick = tick.max(public.deadline());
            assert!(public.start(tick, tick as i64));
            public.finish(tick, tick as i64, Err(Error::Network));
            assert!(public.deadline() - tick <= 3600);
        }
        assert_eq!(public.deadline() - tick, 3600);
    }

    #[test]
    fn disabling_inflight_io_rejects_its_result_even_after_reenabling() {
        use std::sync::mpsc;
        let control = Arc::new(Control {
            inner: Mutex::new(Requested {
                enabled: true,
                interval: 300,
                generation: 1,
                refresh: false,
                stopped: false,
            }),
            changed: Condvar::new(),
        });
        let worker = Worker {
            control: control.clone(),
        };
        let (entered, entry) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (publish, published) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            run(
                control,
                300,
                5,
                move || {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                    Ok("late")
                },
                move |generation, snapshot| publish.send((generation, snapshot)).unwrap(),
            )
        });
        let (old_generation, initial) = published.recv().unwrap();
        assert!(initial.busy);
        entry.recv().unwrap();
        assert!(worker.configure(false, 300));
        assert!(!worker.control.inner.lock().unwrap().accepts(old_generation));
        assert!(!worker.configure(true, 300));
        release.send(()).unwrap();
        let (new_generation, fresh) = published.recv().unwrap();
        assert_ne!(old_generation, new_generation);
        assert!(fresh.busy);
        assert!(fresh.data.is_none());
        entry.recv().unwrap();
        drop(worker);
        release.send(()).unwrap();
        thread.join().unwrap();
        assert!(published.try_recv().is_err());
    }

    #[test]
    fn cadence_changes_do_not_reset_failures_or_evade_rate_limit() {
        let mut job = Schedule::<()>::new(300, 60);
        assert!(job.start(0, 1000));
        job.finish(1, 1001, Err(Error::RateLimit));
        job.configure(60);
        job.request();
        assert_eq!(job.deadline(), 901);
        assert!(!job.start(900, 1900));
        assert!(job.start(901, 1901));
    }
}
