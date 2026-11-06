//! Warm-then-fan-out scheduling for requests that share a prompt-cache prefix.
//!
//! # The defect this closes
//!
//! An Anthropic prompt-cache entry becomes readable only once the request that WRITES it has
//! started to answer. `run_passes` used to put the first 6 rule-batches of a chunk in flight
//! together (`buffer_unordered(6)`), so all six missed the cache and each wrote the same
//! multi-thousand-token prefix at 1.25x. Only rule-batches 7 and up could read it
//! (`SPIKE_TOKEN_EFFICIENCY_RESULTS.md`, items 2 and 3).
//!
//! # The schedule
//!
//! Jobs belong to a prefix group (a chunk: every rule-batch of one chunk shares the repo map and
//! the chunk's digest). The first job of each group is its WARM job:
//!
//! 1. the very first job of the whole run goes alone, so the prefix shared by every group (the
//!    repo map) is written once before anything else reads it;
//! 2. a group's other jobs are held until that group's warm job has returned, then released
//!    together so they all read the cache;
//! 3. while a freshly warmed group has released work, that work is dispatched BEFORE any new
//!    group is warmed, so a cache entry is read soon after it is written (an entry that is
//!    never read within its TTL was paid for for nothing);
//! 4. at most `concurrency` jobs are in flight, and every job runs exactly once.
//!
//! A warm job that fails still releases its siblings: they will write the cache themselves, which
//! costs money but loses nothing, and holding them forever would lose findings.
//!
//! This is scheduling only. It never changes what any request contains.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::future::Future;

use futures::stream::{FuturesUnordered, StreamExt};

/// One unit of work and the prefix group it belongs to.
#[derive(Debug, Clone)]
pub struct ScheduledJob<T> {
    /// Jobs with the same group share a cache prefix. The first job given for a group is its
    /// warm job.
    pub group: usize,
    pub payload: T,
}

/// Run `jobs` through `run`, `concurrency` at a time, warming each group's prefix before its
/// siblings read it. `jobs` are given in priority order (the first job of the first group is
/// the global warm job). Results come back in completion order.
pub async fn run_warm_then_fan_out<T, R, F, Fut>(
    jobs: Vec<ScheduledJob<T>>,
    concurrency: usize,
    run: F,
) -> Vec<R>
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = R>,
{
    let concurrency = concurrency.max(1);

    // Split the jobs into each group's warm job (its first) and the siblings held behind it.
    let mut warm: VecDeque<ScheduledJob<T>> = VecDeque::new();
    let mut held: HashMap<usize, Vec<ScheduledJob<T>>> = HashMap::new();
    for job in jobs {
        match held.entry(job.group) {
            Entry::Occupied(mut siblings) => siblings.get_mut().push(job),
            Entry::Vacant(slot) => {
                slot.insert(Vec::new());
                warm.push_back(job);
            }
        }
    }
    let mut ready: VecDeque<ScheduledJob<T>> = VecDeque::new();
    let mut in_flight = FuturesUnordered::new();
    let mut results = Vec::new();
    let mut anything_returned = false;

    let launch = |job: ScheduledJob<T>, is_warm: bool| {
        let group = job.group;
        let fut = run(job.payload);
        async move { (group, is_warm, fut.await) }
    };

    loop {
        while in_flight.len() < concurrency {
            let next = if anything_returned {
                // Released siblings read a cache entry that is already written, and soonest is
                // best; only then warm another group.
                ready
                    .pop_front()
                    .map(|j| (j, false))
                    .or_else(|| warm.pop_front().map(|j| (j, true)))
            } else if in_flight.is_empty() {
                // The global warm job goes alone: nothing may read the shared prefix before it
                // has been written.
                warm.pop_front().map(|j| (j, true))
            } else {
                None
            };
            let Some((job, is_warm)) = next else { break };
            in_flight.push(launch(job, is_warm));
        }
        let Some((group, was_warm, result)) = in_flight.next().await else {
            break;
        };
        results.push(result);
        anything_returned = true;
        if was_warm {
            // Success or failure, the group's siblings go now (see the module docs).
            if let Some(siblings) = held.remove(&group) {
                ready.extend(siblings);
            }
        }
    }
    results
}

/// The pre-rank-3 schedule, kept behind the `warm_schedule` efficiency switch
/// ([`crate::efficiency::EfficiencyConfig`]): `concurrency` jobs in flight at once in the given
/// order, no warm-up, so a chunk's first rule-batches all go out together. Every job still runs
/// exactly once; results come back in completion order.
pub async fn run_unscheduled<T, R, F, Fut>(
    jobs: Vec<ScheduledJob<T>>,
    concurrency: usize,
    run: F,
) -> Vec<R>
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = R>,
{
    futures::stream::iter(jobs)
        .map(|job| run(job.payload))
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await
}

#[cfg(test)]
mod tests;
