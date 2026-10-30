//! The one scheduler a run uses: for its Bindings, and for each per-row output's rows.
//!
//! Jobs come from an iterator on the calling thread, in order, and run on worker threads. A job
//! names the resources it uses (a Binding, its connections); a resource with a limit runs at
//! most that many jobs at once, and only one until a job using it has finished, so a sign-in (a
//! browser, a token refresh) happens once. A worker takes the first job, in order, that may start
//! now. The producer stays at most `queue` jobs ahead of the workers, so rows are prepared as
//! they're needed, never all at once. With one worker, jobs run on the calling thread.
//!
//! Every finished job's result comes back by its index; a job that never started (the run was
//! cancelled, or a job before it couldn't be prepared) has none.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::engine::CancelToken;

/// A unit of work and the resources it uses.
pub(crate) struct Job<T> {
    pub uses: Vec<String>,
    pub item: T,
}

impl<T> Job<T> {
    /// A job that uses no limited resource (a row).
    pub fn free(item: T) -> Job<T> {
        Job {
            uses: Vec::new(),
            item,
        }
    }
}

/// What [`Scheduler::run`] did: each finished job's result, in job order, and why it stopped
/// taking jobs early, if a job couldn't be prepared.
pub(crate) struct Ran<R, E> {
    pub done: Vec<(usize, R)>,
    pub stopped: Option<E>,
}

/// How jobs run: how many at once, and each resource's limit.
#[derive(Debug, Clone)]
pub(crate) struct Scheduler {
    workers: usize,
    limits: BTreeMap<String, usize>,
    queue: usize,
    /// Run a single worker's jobs on the calling thread.
    here: bool,
}

/// The jobs waiting and running, behind the scheduler's lock.
struct State<T> {
    pending: Vec<(usize, Job<T>)>,
    /// The producer has nothing more to give.
    closed: bool,
    /// Jobs running on each resource.
    on: BTreeMap<String, usize>,
    /// Resources a job has used to the end.
    warm: BTreeSet<String>,
}

/// How often a waiting thread looks at the cancel token, which has no way to wake it.
const POLL: Duration = Duration::from_millis(50);

impl Scheduler {
    /// Up to `workers` jobs at once (at least one).
    pub fn new(workers: usize) -> Scheduler {
        Scheduler {
            workers: workers.max(1),
            limits: BTreeMap::new(),
            queue: usize::MAX,
            here: true,
        }
    }

    /// Always run jobs on worker threads, even with one worker, so the next jobs are prepared
    /// while one runs.
    #[must_use]
    pub fn threaded(mut self) -> Scheduler {
        self.here = false;
        self
    }

    /// At most `n` jobs (at least one) on `resource` at once.
    #[must_use]
    pub fn limit(mut self, resource: &str, n: usize) -> Scheduler {
        self.limits.insert(resource.to_string(), n.max(1));
        self
    }

    /// Prepare at most `n` jobs (at least one) ahead of the workers.
    #[must_use]
    pub fn queue(mut self, n: usize) -> Scheduler {
        self.queue = n.max(1);
        self
    }

    #[cfg(test)]
    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Run `jobs`: each worker makes its state with `worker`, then runs jobs with `work` until
    /// none is left. `work` returning `None` stops that worker (it was cancelled part-way: the
    /// job isn't recorded). A job that fails to be prepared stops the producer: the jobs before
    /// it still run, and its error is [`Ran::stopped`]. No job starts once `cancel` is set.
    ///
    /// # Panics
    ///
    /// Panics if a job panics (a bug), once the other workers have finished theirs.
    pub fn run<T, R, E, W>(
        &self,
        jobs: impl IntoIterator<Item = Result<Job<T>, E>>,
        cancel: &CancelToken,
        worker: impl Fn() -> W + Sync,
        work: impl Fn(&mut W, usize, T) -> Option<R> + Sync,
    ) -> Ran<R, E>
    where
        T: Send,
        R: Send,
    {
        if self.workers == 1 && self.here {
            return self.run_here(jobs, cancel, worker, work);
        }
        let state = Mutex::new(State {
            pending: Vec::new(),
            closed: false,
            on: BTreeMap::new(),
            warm: BTreeSet::new(),
        });
        let wake = Condvar::new();
        let done = Mutex::new(Vec::new());
        let mut stopped = None;
        std::thread::scope(|s| {
            for _ in 0..self.workers {
                s.spawn(|| {
                    let mut w = worker();
                    while let Some((i, job)) = self.take(&state, &wake, cancel) {
                        // Frees the job's resources even if it panics, so the others go on.
                        let _running = Running {
                            scheduler: self,
                            state: &state,
                            wake: &wake,
                            uses: job.uses,
                        };
                        let result = work(&mut w, i, job.item);
                        match result {
                            Some(r) => done.lock().unwrap().push((i, r)),
                            None => break,
                        }
                    }
                    wake.notify_all();
                });
            }
            stopped = self.produce(jobs, &state, &wake, cancel);
        });
        let mut done = done.into_inner().unwrap();
        done.sort_by_key(|(i, _)| *i);
        Ran { done, stopped }
    }

    /// One worker: every job on the calling thread, in order.
    fn run_here<T, R, E, W>(
        &self,
        jobs: impl IntoIterator<Item = Result<Job<T>, E>>,
        cancel: &CancelToken,
        worker: impl Fn() -> W,
        work: impl Fn(&mut W, usize, T) -> Option<R>,
    ) -> Ran<R, E> {
        let mut w = worker();
        let mut done = Vec::new();
        for (i, job) in jobs.into_iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            match job {
                Ok(job) => match work(&mut w, i, job.item) {
                    Some(r) => done.push((i, r)),
                    None => break,
                },
                Err(e) => {
                    return Ran {
                        done,
                        stopped: Some(e),
                    };
                }
            }
        }
        Ran { done, stopped: None }
    }

    /// Hand `jobs` to the workers, staying at most `queue` ahead; the error that stopped it.
    fn produce<T, E>(
        &self,
        jobs: impl IntoIterator<Item = Result<Job<T>, E>>,
        state: &Mutex<State<T>>,
        wake: &Condvar,
        cancel: &CancelToken,
    ) -> Option<E> {
        let mut stopped = None;
        for (i, job) in jobs.into_iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            let job = match job {
                Ok(job) => job,
                Err(e) => {
                    stopped = Some(e);
                    break;
                }
            };
            let mut st = state.lock().unwrap();
            while st.pending.len() >= self.queue && !cancel.is_cancelled() {
                st = wake.wait_timeout(st, POLL).unwrap().0;
            }
            st.pending.push((i, job));
            wake.notify_all();
        }
        state.lock().unwrap().closed = true;
        wake.notify_all();
        stopped
    }

    /// The first pending job that may start now, waiting for one; `None` when there's nothing
    /// left to do, or the run is cancelled.
    fn take<T>(
        &self,
        state: &Mutex<State<T>>,
        wake: &Condvar,
        cancel: &CancelToken,
    ) -> Option<(usize, Job<T>)> {
        let mut st = state.lock().unwrap();
        loop {
            if cancel.is_cancelled() || (st.closed && st.pending.is_empty()) {
                return None;
            }
            if let Some(at) = st.pending.iter().position(|(_, j)| self.may_start(&st, &j.uses)) {
                let (i, job) = st.pending.remove(at);
                for r in &job.uses {
                    *st.on.entry(r.clone()).or_default() += 1;
                }
                wake.notify_all();
                return Some((i, job));
            }
            st = wake.wait_timeout(st, POLL).unwrap().0;
        }
    }

    /// Whether a job using `uses` may start: each limited resource has room, and runs one job
    /// until one has finished on it.
    fn may_start<T>(&self, st: &State<T>, uses: &[String]) -> bool {
        uses.iter().all(|r| {
            let Some(limit) = self.limits.get(r) else {
                return true;
            };
            let on = st.on.get(r).copied().unwrap_or(0);
            on < if st.warm.contains(r) { *limit } else { 1 }
        })
    }

    fn finish<T>(&self, state: &Mutex<State<T>>, wake: &Condvar, uses: &[String]) {
        // The lock is never held while a job runs, so a job's panic can't poison it.
        let mut st = state.lock().unwrap();
        for r in uses {
            *st.on.entry(r.clone()).or_default() -= 1;
            st.warm.insert(r.clone());
        }
        wake.notify_all();
    }
}

/// A job running: its resources are freed when it ends, however it ends.
struct Running<'s, T> {
    scheduler: &'s Scheduler,
    state: &'s Mutex<State<T>>,
    wake: &'s Condvar,
    uses: Vec<String>,
}

impl<T> Drop for Running<'_, T> {
    fn drop(&mut self) {
        self.scheduler.finish(self.state, self.wake, &self.uses);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;

    fn jobs<T>(items: impl IntoIterator<Item = (Vec<&'static str>, T)>) -> Vec<Result<Job<T>, String>> {
        items
            .into_iter()
            .map(|(uses, item)| {
                Ok(Job {
                    uses: uses.into_iter().map(str::to_string).collect(),
                    item,
                })
            })
            .collect()
    }

    /// The most jobs that ran at once, measured by the jobs themselves.
    #[derive(Default)]
    struct Peak {
        now: AtomicUsize,
        most: AtomicUsize,
    }

    impl Peak {
        fn run(&self, pause: Duration) {
            let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(pause);
            self.now.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn every_job_runs_once_and_results_come_back_in_job_order() {
        let ran = Scheduler::new(4).run(
            jobs((0..20).map(|i| (vec![], i))),
            &CancelToken::new(),
            || (),
            |_, _, n: i32| Some(n * 10),
        );
        let got: Vec<(usize, i32)> = ran.done;
        assert_eq!(got, (0..20).map(|i| (i as usize, i * 10)).collect::<Vec<_>>());
        assert!(ran.stopped.is_none());
    }

    #[test]
    fn a_resource_never_runs_more_jobs_than_its_limit() {
        let peak = Peak::default();
        let ran = Scheduler::new(8).limit("warehouse", 2).run(
            jobs((0..8).map(|i| (vec!["warehouse"], i))),
            &CancelToken::new(),
            || (),
            |_, _, _| {
                peak.run(Duration::from_millis(20));
                Some(())
            },
        );
        assert_eq!(ran.done.len(), 8);
        assert!(peak.most.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn a_resource_runs_one_job_until_one_has_finished_on_it() {
        // Two jobs may share the connection, but the first runs alone: whichever job a worker
        // takes first, nothing else is running when it starts.
        let running = AtomicUsize::new(0);
        let first_alone = Mutex::new(None);
        Scheduler::new(4).limit("warehouse", 4).run(
            jobs((0..4).map(|i| (vec!["warehouse"], i))),
            &CancelToken::new(),
            || (),
            |_, _, _| {
                let others = running.fetch_add(1, Ordering::SeqCst);
                first_alone.lock().unwrap().get_or_insert(others == 0);
                std::thread::sleep(Duration::from_millis(10));
                running.fetch_sub(1, Ordering::SeqCst);
                Some(())
            },
        );
        assert_eq!(*first_alone.lock().unwrap(), Some(true));
    }

    #[test]
    fn jobs_on_other_resources_run_while_one_is_busy() {
        // Two workers; the first job holds `a` until `b`'s job has run, which only works if the
        // second worker can start `b` while `a` is busy.
        let both = Barrier::new(2);
        let ran = Scheduler::new(2).limit("a", 1).limit("b", 1).run(
            jobs([(vec!["a"], 0), (vec!["a"], 1), (vec!["b"], 2)]),
            &CancelToken::new(),
            || (),
            |_, _, n| {
                if n != 1 {
                    both.wait();
                }
                Some(n)
            },
        );
        assert_eq!(ran.done.iter().map(|(_, n)| *n).collect::<Vec<_>>(), [0, 1, 2]);
    }

    #[test]
    fn the_producer_stays_at_most_its_queue_ahead() {
        let made = AtomicUsize::new(0);
        let started = AtomicUsize::new(0);
        let most_ahead = AtomicUsize::new(0);
        let produce = (0..30).map(|i| {
            let ahead = made.fetch_add(1, Ordering::SeqCst) + 1 - started.load(Ordering::SeqCst);
            most_ahead.fetch_max(ahead, Ordering::SeqCst);
            Ok::<_, String>(Job::free(i))
        });
        Scheduler::new(2).queue(3).run(
            produce,
            &CancelToken::new(),
            || (),
            |_, _, _| {
                started.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(2));
                Some(())
            },
        );
        // Queued, plus one being made, plus one each worker took but hasn't counted yet.
        assert!(most_ahead.load(Ordering::SeqCst) <= 3 + 1 + 2);
    }

    #[test]
    fn a_job_that_cant_be_prepared_stops_the_rest_but_not_those_before_it() {
        for workers in [1, 3] {
            let mut items: Vec<Result<Job<i32>, String>> = jobs((0..3).map(|i| (vec![], i)));
            items.push(Err("row 4 doesn't render".into()));
            items.extend(jobs((4..6).map(|i| (vec![], i))));
            let ran = Scheduler::new(workers).run(items, &CancelToken::new(), || (), |_, _, n| Some(n));
            assert_eq!(ran.done.iter().map(|(_, n)| *n).collect::<Vec<_>>(), [0, 1, 2]);
            assert_eq!(ran.stopped.as_deref(), Some("row 4 doesn't render"));
        }
    }

    #[test]
    fn no_job_starts_once_the_run_is_cancelled() {
        for workers in [1, 3] {
            let cancel = CancelToken::new();
            let ran = Scheduler::new(workers).run(
                jobs((0..50).map(|i| (vec![], i))),
                &cancel,
                || (),
                |_, _, n| {
                    if n == 0 {
                        cancel.cancel();
                    }
                    Some(n)
                },
            );
            // Each worker may have taken one job before it saw the cancel.
            assert!(ran.done.len() <= workers, "{}", ran.done.len());
        }
    }

    #[test]
    fn a_job_that_gives_up_stops_its_worker_unrecorded() {
        let ran = Scheduler::new(1).run(
            jobs((0..5).map(|i| (vec![], i))),
            &CancelToken::new(),
            || (),
            |_, _, n| (n < 2).then_some(n),
        );
        assert_eq!(ran.done.iter().map(|(_, n)| *n).collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn each_worker_keeps_its_own_state_across_jobs() {
        let made = AtomicUsize::new(0);
        let ran = Scheduler::new(3).run(
            jobs((0..12).map(|i| (vec![], i))),
            &CancelToken::new(),
            || {
                made.fetch_add(1, Ordering::SeqCst);
                Arc::new(AtomicUsize::new(0))
            },
            |w, _, _| Some(w.fetch_add(1, Ordering::SeqCst)),
        );
        assert!(made.load(Ordering::SeqCst) <= 3);
        assert_eq!(ran.done.len(), 12);
    }

    #[test]
    fn a_job_that_panics_frees_its_resource_and_the_run_still_ends() {
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Scheduler::new(2).limit("a", 1).run(
                jobs((0..4).map(|i| (vec!["a"], i))),
                &CancelToken::new(),
                || (),
                |_, _, n| {
                    assert_ne!(n, 0, "the first job breaks");
                    Some(n)
                },
            )
        }));
        assert!(ran.is_err());
    }

    #[test]
    fn a_threaded_scheduler_runs_even_one_worker_off_the_calling_thread() {
        let here = std::thread::current().id();
        let ran = Scheduler::new(1).threaded().run(
            jobs((0..3).map(|i| (vec![], i))),
            &CancelToken::new(),
            || (),
            |_, _, _| Some(std::thread::current().id()),
        );
        assert_eq!(ran.done.len(), 3);
        assert!(ran.done.iter().all(|(_, id)| *id != here));
    }

    #[test]
    fn one_worker_runs_jobs_on_the_calling_thread() {
        let here = std::thread::current().id();
        let ran = Scheduler::new(1).run(
            jobs((0..3).map(|i| (vec!["a"], i))),
            &CancelToken::new(),
            || (),
            |_, _, _| Some(std::thread::current().id()),
        );
        assert!(ran.done.iter().all(|(_, id)| *id == here));
    }
}
