//! [`InMemorySchedulerStore`] and [`assert_scheduler_store_contract`].
//!
//! **Availability:** since `0.17.0` · **not wasm-supported** (native only; requires `scheduler`)

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::error::CanoError;
use crate::scheduler::{CasOutcome, SchedulerStore, VersionedRecord};

/// A process-local [`SchedulerStore`].
///
/// **Availability:** since `0.17.0` · **not wasm-supported** (native only)
///
/// Share one `Arc<InMemorySchedulerStore>` between several schedulers in one test to
/// exercise multi-instance coordination without a database. It honors the full trait
/// contract; versions count up from `1` across the whole store.
#[derive(Default, Debug)]
pub struct InMemorySchedulerStore {
    inner: Mutex<(u64, HashMap<String, VersionedRecord>)>,
}

impl InMemorySchedulerStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current record for `flow_id`, if any.
    pub fn record(&self, flow_id: &str) -> Option<VersionedRecord> {
        self.inner.lock().1.get(flow_id).cloned()
    }
}

#[crate::scheduler_store]
impl SchedulerStore for InMemorySchedulerStore {
    async fn load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError> {
        Ok(self.record(flow_id))
    }

    async fn compare_and_swap(
        &self,
        flow_id: &str,
        expected: Option<u64>,
        value: Vec<u8>,
    ) -> Result<CasOutcome, CanoError> {
        let mut guard = self.inner.lock();
        let (counter, records) = &mut *guard;
        if records.get(flow_id).map(|r| r.version) != expected {
            return Ok(CasOutcome::Conflict);
        }
        *counter = counter
            .checked_add(1)
            .ok_or_else(|| CanoError::scheduler_store("scheduler version counter overflow"))?;
        records.insert(
            flow_id.to_string(),
            VersionedRecord {
                version: *counter,
                value,
            },
        );
        Ok(CasOutcome::Written(*counter))
    }

    async fn remove(&self, flow_id: &str) -> Result<(), CanoError> {
        self.inner.lock().1.remove(flow_id);
        Ok(())
    }
}

/// Run the [`SchedulerStore`] contract against `store`.
///
/// Use it to test your own backend, or a built-in one against a real server. It uses only
/// flow ids of the form `{namespace}/{case}`, removes each of them before and after use,
/// and leaves every other id alone, so it can run against a shared table or bucket. Pick a
/// `namespace` no scheduler uses.
///
/// The clauses: an unknown id loads `None` and removes as a no-op; a create (`expected =
/// None`) writes once and conflicts afterwards; a swap from the current version writes and
/// changes the version; a swap from a stale or wrong version conflicts and leaves the record
/// alone; a removed id can be created again with a version different from its earlier ones,
/// and pre-remove versions still conflict; ids that share a prefix (`a`, `a.b`, `a/b`, `ab`)
/// stay isolated; ids with spaces, wildcards, non-ASCII and 200+ characters work; of
/// concurrent creates (including post-`remove` races with equal bytes), and of concurrent
/// swaps from one version, exactly one is written and matches what `load` reads back;
/// concurrent `load`, `compare_and_swap`, and `remove` stay linearizable; a 256 KiB value
/// and an empty value round-trip.
///
/// # Panics
///
/// Panics with a message naming the violated clause, or the store error that interrupted it.
///
/// ```rust
/// use cano::testing::{InMemorySchedulerStore, assert_scheduler_store_contract};
///
/// # #[tokio::main]
/// # async fn main() {
/// let store = InMemorySchedulerStore::new();
/// assert_scheduler_store_contract(&store, "contract").await;
/// # }
/// ```
pub async fn assert_scheduler_store_contract(store: &dyn SchedulerStore, namespace: &str) {
    let id = |case: &str| format!("{namespace}/{case}");
    let isolated = ["a", "a.b", "a=2Eb", "a_b", "axb", "a/b", "ab"].map(id);
    let awkward = [
        id(" spaced id"),
        id("wild*card>"),
        id("ünï©ode-✓"),
        id("per%cent=eq"),
        id(".dot."),
        id("'quoted'"),
        id(&"x".repeat(200)),
    ];
    let mut ids: Vec<String> = [
        "unknown",
        "create",
        "swap",
        "empty",
        "recreate",
        "race-new",
        "race-swap",
        "race-tombstone",
        "concurrent-ops",
        "big",
    ]
    .into_iter()
    .map(id)
    .collect();
    ids.extend(isolated.iter().cloned());
    ids.extend(awkward.iter().cloned());
    for flow_id in &ids {
        must("setup", "remove", store.remove(flow_id)).await;
    }

    // 1. Unknown id.
    let unknown = id("unknown");
    let loaded = must("unknown id", "load", store.load(&unknown)).await;
    assert!(
        loaded.is_none(),
        "scheduler store contract [unknown id]: load returned {loaded:?}"
    );
    must("unknown id", "remove", store.remove(&unknown)).await;

    // 2. Create once.
    let create = id("create");
    let v1 = written(
        "create",
        must(
            "create",
            "compare_and_swap",
            store.compare_and_swap(&create, None, b"one".to_vec()),
        )
        .await,
    );
    expect_record(store, "create", &create, v1, b"one").await;
    let again = must(
        "create",
        "compare_and_swap",
        store.compare_and_swap(&create, None, b"two".to_vec()),
    )
    .await;
    assert_eq!(
        again,
        CasOutcome::Conflict,
        "scheduler store contract [create]: a second create of one id must conflict"
    );
    expect_record(store, "create", &create, v1, b"one").await;

    // 3. Swap from the current version; stale and wrong versions conflict.
    let swap = id("swap");
    let s1 = written(
        "swap",
        must(
            "swap",
            "compare_and_swap",
            store.compare_and_swap(&swap, None, b"a".to_vec()),
        )
        .await,
    );
    let s2 = written(
        "swap",
        must(
            "swap",
            "compare_and_swap",
            store.compare_and_swap(&swap, Some(s1), b"b".to_vec()),
        )
        .await,
    );
    assert_ne!(
        s1, s2,
        "scheduler store contract [swap]: a write must change the version"
    );
    expect_record(store, "swap", &swap, s2, b"b").await;
    for (what, expected) in [
        ("stale", s1),
        ("wrong", s2.wrapping_add(1_000_003)),
        ("zero", 0),
        ("max", u64::MAX),
    ] {
        let outcome = must(
            "swap",
            "compare_and_swap",
            store.compare_and_swap(&swap, Some(expected), b"c".to_vec()),
        )
        .await;
        assert_eq!(
            outcome,
            CasOutcome::Conflict,
            "scheduler store contract [swap]: a swap from a {what} version must conflict"
        );
    }
    for (what, expected) in [("current", s2), ("zero", 0), ("max", u64::MAX)] {
        let missing = must(
            "swap",
            "compare_and_swap",
            store.compare_and_swap(&id("unknown"), Some(expected), b"c".to_vec()),
        )
        .await;
        assert_eq!(
            missing,
            CasOutcome::Conflict,
            "scheduler store contract [swap]: a swap ({what}) on a missing id must conflict"
        );
    }
    assert!(
        must("swap", "load", store.load(&id("unknown")))
            .await
            .is_none(),
        "scheduler store contract [swap]: a conflicting swap on a missing id created a record"
    );
    expect_record(store, "swap", &swap, s2, b"b").await;

    // 4. Remove, then create again.
    let recreate = id("recreate");
    let r1 = written(
        "recreate",
        must(
            "recreate",
            "compare_and_swap",
            store.compare_and_swap(&recreate, None, b"x".to_vec()),
        )
        .await,
    );
    must("recreate", "remove", store.remove(&recreate)).await;
    let gone = must("recreate", "load", store.load(&recreate)).await;
    assert!(
        gone.is_none(),
        "scheduler store contract [recreate]: load after remove returned {gone:?}"
    );
    let r2 = written(
        "recreate",
        must(
            "recreate",
            "compare_and_swap",
            store.compare_and_swap(&recreate, None, b"y".to_vec()),
        )
        .await,
    );
    assert_ne!(
        r1, r2,
        "scheduler store contract [recreate]: a re-created record must not reuse an old version"
    );
    expect_record(store, "recreate", &recreate, r2, b"y").await;
    let stale_after_recreate = must(
        "recreate",
        "compare_and_swap stale pre-remove version",
        store.compare_and_swap(&recreate, Some(r1), b"z".to_vec()),
    )
    .await;
    assert_eq!(
        stale_after_recreate,
        CasOutcome::Conflict,
        "scheduler store contract [recreate]: pre-remove version must conflict after recreate"
    );
    expect_record(store, "recreate", &recreate, r2, b"y").await;

    // 5. Isolation, including awkward ids.
    for flow_id in isolated.iter().chain(awkward.iter()) {
        written(
            "isolation",
            must(
                "isolation",
                "compare_and_swap",
                store.compare_and_swap(flow_id, None, flow_id.as_bytes().to_vec()),
            )
            .await,
        );
    }
    for flow_id in isolated.iter().chain(awkward.iter()) {
        let rec = must("isolation", "load", store.load(flow_id))
            .await
            .unwrap_or_else(|| {
                panic!("scheduler store contract [isolation]: {flow_id:?} is missing")
            });
        assert_eq!(
            rec.value,
            flow_id.as_bytes(),
            "scheduler store contract [isolation]: {flow_id:?} holds another id's value"
        );
    }
    must("isolation", "remove", store.remove(&isolated[0])).await;
    for flow_id in &isolated[1..] {
        assert!(
            must("isolation", "load", store.load(flow_id))
                .await
                .is_some(),
            "scheduler store contract [isolation]: removing {:?} removed {flow_id:?}",
            isolated[0]
        );
    }

    // 6. Races: exactly one of concurrent creates, of concurrent swaps, and of
    // post-remove creates with equal bytes wins, and `load` sees the winner's payload.
    let race_new = id("race-new");
    let outcomes = race(store, &race_new, None).await;
    let (winner_idx, winner_ver) = assert_one_winner("race on create", &outcomes);
    expect_record(
        store,
        "race on create",
        &race_new,
        winner_ver,
        &[b'r', winner_idx as u8],
    )
    .await;

    let race_swap = id("race-swap");
    let base = written(
        "race on swap",
        must(
            "race on swap",
            "compare_and_swap",
            store.compare_and_swap(&race_swap, None, b"base".to_vec()),
        )
        .await,
    );
    let outcomes = race(store, &race_swap, Some(base)).await;
    let (winner_idx, winner_ver) = assert_one_winner("race on swap", &outcomes);
    expect_record(
        store,
        "race on swap",
        &race_swap,
        winner_ver,
        &[b'r', winner_idx as u8],
    )
    .await;

    let race_tombstone = id("race-tombstone");
    let t_prev = written(
        "race after remove",
        must(
            "race after remove",
            "seed",
            store.compare_and_swap(&race_tombstone, None, b"seed".to_vec()),
        )
        .await,
    );
    must("race after remove", "remove", store.remove(&race_tombstone)).await;
    let equal_payload = b"same-after-remove".to_vec();
    let calls =
        (0..8).map(|_| store.compare_and_swap(&race_tombstone, None, equal_payload.clone()));
    let outcomes: Vec<CasOutcome> = futures_util::future::join_all(calls)
        .await
        .into_iter()
        .map(|r| {
            r.unwrap_or_else(|e| {
                panic!("scheduler store contract [race after remove]: compare_and_swap failed: {e}")
            })
        })
        .collect();
    let (_, t_ver) = assert_one_winner("race after remove", &outcomes);
    assert_ne!(
        t_ver, t_prev,
        "scheduler store contract [race after remove]: recreated version must not repeat"
    );
    expect_record(
        store,
        "race after remove",
        &race_tombstone,
        t_ver,
        &equal_payload,
    )
    .await;

    // 7. A 256 KiB value, empty value, and identical-value version bump.
    let big = id("big");
    let value: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();
    let v = written(
        "large value",
        must(
            "large value",
            "compare_and_swap",
            store.compare_and_swap(&big, None, value.clone()),
        )
        .await,
    );
    expect_record(store, "large value", &big, v, &value).await;

    let empty = id("empty");
    let e1 = written(
        "empty value",
        must(
            "empty value",
            "compare_and_swap",
            store.compare_and_swap(&empty, None, Vec::new()),
        )
        .await,
    );
    expect_record(store, "empty value", &empty, e1, &[]).await;
    let e2 = written(
        "same value swap",
        must(
            "same value swap",
            "compare_and_swap",
            store.compare_and_swap(&empty, Some(e1), Vec::new()),
        )
        .await,
    );
    assert_ne!(
        e1, e2,
        "scheduler store contract [same value swap]: swapping identical bytes must bump version"
    );
    let stale_same = must(
        "same value swap",
        "compare_and_swap stale",
        store.compare_and_swap(&empty, Some(e1), Vec::new()),
    )
    .await;
    assert_eq!(
        stale_same,
        CasOutcome::Conflict,
        "scheduler store contract [same value swap]: stale version with identical bytes must conflict"
    );
    expect_record(store, "empty value", &empty, e2, &[]).await;

    // 8. Concurrent `load`, `compare_and_swap`, and `remove` stay linearizable without error.
    let concurrent_ops = id("concurrent-ops");
    let seed_v = written(
        "concurrent load+swap+remove",
        must(
            "concurrent load+swap+remove",
            "seed",
            store.compare_and_swap(&concurrent_ops, None, b"seed".to_vec()),
        )
        .await,
    );
    let loads = futures_util::future::join_all((0..4).map(|_| store.load(&concurrent_ops)));
    let swaps = futures_util::future::join_all(
        (0..4u8).map(|i| store.compare_and_swap(&concurrent_ops, Some(seed_v), vec![b's', i])),
    );
    let removes = futures_util::future::join_all((0..2).map(|_| store.remove(&concurrent_ops)));
    let (load_results, swap_results, remove_results) =
        futures_util::future::join3(loads, swaps, removes).await;

    for res in load_results {
        let rec = res.unwrap_or_else(|e| {
            panic!("scheduler store contract [concurrent load+swap+remove]: load failed: {e}")
        });
        if let Some(rec) = rec {
            let valid_payload = rec.value == b"seed"
                || (rec.value.len() == 2 && rec.value[0] == b's' && rec.value[1] < 4);
            assert!(
                valid_payload,
                "scheduler store contract [concurrent load+swap+remove]: unexpected value {:?}",
                rec.value
            );
        }
    }
    let mut swap_wins = 0usize;
    for res in swap_results {
        match res.unwrap_or_else(|e| {
            panic!("scheduler store contract [concurrent load+swap+remove]: swap failed: {e}")
        }) {
            CasOutcome::Written(_) => swap_wins += 1,
            CasOutcome::Conflict => {}
        }
    }
    assert!(
        swap_wins <= 1,
        "scheduler store contract [concurrent load+swap+remove]: {swap_wins} swaps from seed version succeeded"
    );
    for res in remove_results {
        res.unwrap_or_else(|e| {
            panic!("scheduler store contract [concurrent load+swap+remove]: remove failed: {e}")
        });
    }
    must(
        "concurrent load+swap+remove",
        "final remove",
        store.remove(&concurrent_ops),
    )
    .await;
    assert!(
        must(
            "concurrent load+swap+remove",
            "load after remove",
            store.load(&concurrent_ops)
        )
        .await
        .is_none(),
        "scheduler store contract [concurrent load+swap+remove]: record remained after remove"
    );
    let recreated_after = written(
        "concurrent load+swap+remove",
        must(
            "concurrent load+swap+remove",
            "create after remove",
            store.compare_and_swap(&concurrent_ops, None, b"after".to_vec()),
        )
        .await,
    );
    assert_ne!(recreated_after, seed_v);
    expect_record(
        store,
        "concurrent load+swap+remove",
        &concurrent_ops,
        recreated_after,
        b"after",
    )
    .await;

    for flow_id in &ids {
        must("cleanup", "remove", store.remove(flow_id)).await;
    }
}

/// Eight concurrent swaps of distinct values for `flow_id` from `expected`.
async fn race(store: &dyn SchedulerStore, flow_id: &str, expected: Option<u64>) -> Vec<CasOutcome> {
    let calls = (0u8..8).map(|i| store.compare_and_swap(flow_id, expected, vec![b'r', i]));
    futures_util::future::join_all(calls)
        .await
        .into_iter()
        .map(|r| {
            r.unwrap_or_else(|e| {
                panic!("scheduler store contract [race]: compare_and_swap failed: {e}")
            })
        })
        .collect()
}

fn assert_one_winner(clause: &str, outcomes: &[CasOutcome]) -> (usize, u64) {
    let winners: Vec<(usize, u64)> = outcomes
        .iter()
        .enumerate()
        .filter_map(|(idx, o)| match o {
            CasOutcome::Written(ver) => Some((idx, *ver)),
            CasOutcome::Conflict => None,
        })
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "scheduler store contract [{clause}]: {} of {} concurrent swaps were written",
        winners.len(),
        outcomes.len()
    );
    winners[0]
}

fn written(clause: &str, outcome: CasOutcome) -> u64 {
    match outcome {
        CasOutcome::Written(version) => version,
        CasOutcome::Conflict => {
            panic!("scheduler store contract [{clause}]: expected a write, got a conflict")
        }
    }
}

async fn expect_record(
    store: &dyn SchedulerStore,
    clause: &str,
    flow_id: &str,
    version: u64,
    value: &[u8],
) {
    let rec = must(clause, "load", store.load(flow_id)).await;
    let expected = VersionedRecord {
        version,
        value: value.to_vec(),
    };
    assert!(
        rec.as_ref() == Some(&expected),
        "scheduler store contract [{clause}]: expected version {version} with {} bytes, got {:?}",
        value.len(),
        rec.map(|r| (r.version, r.value.len()))
    );
}

async fn must<T>(clause: &str, what: &str, call: impl Future<Output = Result<T, CanoError>>) -> T {
    call.await
        .unwrap_or_else(|e| panic!("scheduler store contract [{clause}]: {what} failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_store_passes_the_contract() {
        assert_scheduler_store_contract(&InMemorySchedulerStore::new(), "contract").await;
    }

    #[test]
    fn scheduler_store_is_dyn_compatible() {
        let _erased: std::sync::Arc<dyn SchedulerStore> =
            std::sync::Arc::new(InMemorySchedulerStore::new());
    }
}
