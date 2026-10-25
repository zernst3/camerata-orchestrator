use std::sync::Mutex;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ev {
    Start(usize, usize),
    End(usize, usize),
}

/// A log of job starts and ends; `(group, index-in-group)` names a job.
#[derive(Default)]
struct Log {
    events: Mutex<Vec<Ev>>,
    in_flight: Mutex<(usize, usize)>, // (current, max)
}

impl Log {
    fn start(&self, g: usize, i: usize) {
        self.events.lock().unwrap().push(Ev::Start(g, i));
        let mut f = self.in_flight.lock().unwrap();
        f.0 += 1;
        f.1 = f.1.max(f.0);
    }
    fn end(&self, g: usize, i: usize) {
        self.events.lock().unwrap().push(Ev::End(g, i));
        self.in_flight.lock().unwrap().0 -= 1;
    }
    fn pos(&self, ev: Ev) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .position(|e| *e == ev)
            .unwrap_or_else(|| panic!("{ev:?} never happened"))
    }
    fn max_in_flight(&self) -> usize {
        self.in_flight.lock().unwrap().1
    }
}

/// `n_groups` groups of `per_group` jobs each, in chunk-major order (the order `run_passes`
/// builds).
fn jobs(n_groups: usize, per_group: usize) -> Vec<ScheduledJob<(usize, usize)>> {
    (0..n_groups)
        .flat_map(|g| {
            (0..per_group).map(move |i| ScheduledJob {
                group: g,
                payload: (g, i),
            })
        })
        .collect()
}

/// Run the jobs; each yields a few times so concurrently running jobs interleave.
async fn run(
    log: &Log,
    jobs: Vec<ScheduledJob<(usize, usize)>>,
    concurrency: usize,
) -> Vec<(usize, usize)> {
    run_warm_then_fan_out(jobs, concurrency, |(g, i)| async move {
        log.start(g, i);
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        log.end(g, i);
        (g, i)
    })
    .await
}

#[tokio::test]
async fn the_first_job_of_the_run_completes_before_anything_else_starts() {
    let log = Log::default();
    run(&log, jobs(3, 4), 6).await;
    let first_end = log.pos(Ev::End(0, 0));
    for (g, i) in [(0, 1), (0, 3), (1, 0), (2, 3)] {
        assert!(
            first_end < log.pos(Ev::Start(g, i)),
            "job ({g},{i}) started before the global warm job returned"
        );
    }
}

#[tokio::test]
async fn a_groups_siblings_start_only_after_its_warm_job_returned() {
    let log = Log::default();
    run(&log, jobs(4, 5), 6).await;
    for g in 0..4 {
        let warm_end = log.pos(Ev::End(g, 0));
        for i in 1..5 {
            assert!(
                warm_end < log.pos(Ev::Start(g, i)),
                "group {g}: sibling {i} started before the warm job returned"
            );
        }
    }
}

#[tokio::test]
async fn siblings_fan_out_together_after_the_warm_job() {
    // One chunk, six rule-batches, six slots: after the warm job all five siblings must be in
    // flight at once (the fan-out), not trickle through.
    let log = Log::default();
    run(&log, jobs(1, 6), 6).await;
    assert_eq!(log.max_in_flight(), 5, "five siblings fan out together");
}

#[tokio::test]
async fn concurrency_is_never_exceeded_and_every_job_runs_exactly_once() {
    let log = Log::default();
    let mut done = run(&log, jobs(5, 7), 3).await;
    assert!(log.max_in_flight() <= 3, "{}", log.max_in_flight());
    assert!(log.max_in_flight() >= 2, "work was actually parallel");
    done.sort_unstable();
    let expected: Vec<(usize, usize)> = (0..5).flat_map(|g| (0..7).map(move |i| (g, i))).collect();
    assert_eq!(done, expected);
}

#[tokio::test]
async fn released_siblings_are_dispatched_before_a_new_group_is_warmed() {
    // Two slots. When group 0's warm job returns, its siblings (read the fresh cache) must take
    // the slots ahead of group 1's warm job, so an entry is read soon after it is written.
    let log = Log::default();
    run(&log, jobs(2, 3), 2).await;
    let warm0_end = log.pos(Ev::End(0, 0));
    let sib1 = log.pos(Ev::Start(0, 1));
    let sib2 = log.pos(Ev::Start(0, 2));
    let warm1 = log.pos(Ev::Start(1, 0));
    assert!(warm0_end < sib1 && warm0_end < sib2);
    assert!(
        sib1 < warm1 && sib2 < warm1,
        "group 0's siblings must start before group 1 is warmed: {:?}",
        log.events.lock().unwrap()
    );
}

#[tokio::test]
async fn a_failed_warm_job_still_releases_its_siblings() {
    let results = run_warm_then_fan_out(jobs(1, 4), 4, |(g, i)| async move {
        tokio::task::yield_now().await;
        if (g, i) == (0, 0) {
            Err("transport error")
        } else {
            Ok((g, i))
        }
    })
    .await;
    assert_eq!(
        results.len(),
        4,
        "no sibling may be lost to a failed warm job"
    );
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 3);
}

#[tokio::test]
async fn one_slot_runs_everything_in_the_given_order() {
    let log = Log::default();
    let done = run(&log, jobs(2, 2), 1).await;
    assert_eq!(done, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
}

#[tokio::test]
async fn no_jobs_is_not_an_error() {
    let log = Log::default();
    assert!(run(&log, Vec::new(), 6).await.is_empty());
}

#[tokio::test]
async fn a_zero_concurrency_still_makes_progress() {
    let log = Log::default();
    assert_eq!(run(&log, jobs(1, 2), 0).await.len(), 2);
}
