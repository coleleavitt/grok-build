//! Event ID generation for session notifications.
//!
//! Provides a globally unique event ID format `{session_id}-{counter}` that is
//! used for deduplication in the relay. The counter is monotonically increasing
//! across the entire agent process, ensuring event IDs are always comparable.

use std::sync::atomic::{AtomicU64, Ordering};

/// Global counter for event ID generation.
/// Shared across all sessions to ensure monotonically increasing IDs.
static EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Serializes "mint an id" with "publish the copies that carry it".
///
/// Process-global, like [`EVENT_COUNTER`]: a session's durable log and its
/// live stream are both fed by unbounded MPSC channels, so the order two
/// producers *reach* those channels is the order they land — and that has to
/// agree with the id order, not merely be close to it.
static EVENT_ORDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Mint-and-publish an event as one indivisible step.
///
/// `updates.jsonl` order is decided by the order producers reach the
/// persistence channel. Stamping and enqueuing as separate steps lets a
/// producer that minted `N` lose the race to a concurrent producer that minted
/// `N+1`, writing `N+1` ahead of `N` on disk. Running the stamp and every
/// enqueue it feeds (persistence and gateway) under this lock keeps file order,
/// broadcast order, and `eventId` order identical for producers that route
/// through it — see [`ensure_event_id_meta`] for the list.
///
/// It does NOT make that a global invariant, and must not be relied on as one.
/// The buffered streaming path deliberately opts out: `send_update_full` mints
/// at ENQUEUE, because the id order has to match *delivery* order for the
/// client's in-order dedup, while the append happens later — after
/// `ReplayBuffer` merge/debounce. A concurrent producer therefore lands a
/// higher id earlier in the file as a matter of course, not as a race.
/// `prepare_replay_lines` is what closes the loop: it selects the replay tail
/// by `eventId` counter rather than by file position, so an interleaved append
/// can never strand an event ahead of a client's cursor. This lock narrows the
/// window; the counter ordering is the correctness boundary.
///
/// `publish` runs with the lock held, so it must only enqueue — it is `FnOnce`
/// and synchronous by construction (no `.await` can appear inside), and it
/// must not itself call [`with_event_order`]: the lock is not reentrant.
pub fn with_event_order<T>(publish: impl FnOnce() -> T) -> T {
    // A panic inside `publish` leaves nothing to corrupt — the guarded state
    // is `()`, the counter is atomic — so poison is recovered, not propagated.
    let _order = EVENT_ORDER.lock().unwrap_or_else(|e| e.into_inner());
    publish()
}

/// Generates a unique event ID for correlation across agent/relay/client.
///
/// Format: `{session_id}-{counter}` where counter is a monotonically increasing
/// global counter. This format allows the relay to compare event IDs numerically
/// by extracting the counter suffix.
///
/// # Arguments
/// * `session_id` - The session ID to include in the event ID
///
/// # Returns
/// A unique event ID string in the format `{session_id}-{counter}`
pub fn generate_event_id(session_id: &str) -> String {
    let count = EVENT_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{}-{}", session_id, count)
}

/// Stamp `_meta.eventId` (+ `agentTimestampMs`) onto a notification's meta
/// unless an `eventId` is already present, preserving any other meta fields.
///
/// Every PERSISTED notification should carry an `eventId`: the reconnect
/// cursor (`session/load` `_meta.cursor`) can only bound the replay tail when
/// each persisted line is identifiable, and the same id must ride the live
/// broadcast so clients advance their cursor to ids that exist on disk.
/// Broadcast-only notifications are deliberately left unstamped — a cursor
/// pointing at an id absent from `updates.jsonl` never resolves and forces a
/// full replay on every reconnect.
///
/// Stamping chokepoints (stamp BEFORE the persist/broadcast fork, so both
/// copies share one id, and stamp INSIDE [`with_event_order`] so the id order
/// is also the on-disk order): `SessionActor::emit_notification_direct` (all
/// actor ACP notifications, incl. the buffered pipeline),
/// `send_xai_notification` / `persist_xai_update_only` /
/// `handle_xai_session_notification` (actor xAI — the last one is also the
/// sole owner of subagent-lifecycle persist+broadcast, which `subagent::
/// emit_subagent_notification` hands to it), `notification_bridge::
/// stamp_event_id` (bridge), `GoalNotifySender::dispatch_update` (goal mode),
/// `workflow::notify::WorkflowNotifier::dispatch`, plus the inline
/// `build_notification_meta` user-echo persists. An emitter outside these is
/// not a correctness bug — `prepare_replay_lines` refuses cursors over id-less
/// tails (full replay, safe) — but it silently disables incremental reconnect
/// for affected sessions.
///
/// The converse is a real bug: stamping a notification that is NOT persisted
/// hands the client a cursor id that no line on disk carries, so every
/// reconnect falls back to a full replay. Transient emitters (`SubagentProgress`
/// ticks, `emit_goal_updated_ephemeral`, non-persisting workflow broadcasts)
/// must therefore stay unstamped.
pub fn ensure_event_id_meta(
    session_id: &str,
    meta: &mut Option<serde_json::Map<String, serde_json::Value>>,
) {
    if meta
        .as_ref()
        .and_then(|m| m.get("eventId"))
        .is_some_and(|v| !v.is_null())
    {
        return;
    }
    let event_id = generate_event_id(session_id);
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let obj = meta.get_or_insert_with(serde_json::Map::new);
    obj.insert("eventId".into(), event_id.into());
    obj.entry("agentTimestampMs")
        .or_insert_with(|| timestamp_ms.into());
}

/// Raise the global event counter so the next generated id is at least `next`.
///
/// The counter is process-global and starts at 0 on every launch, but the
/// monotonic-`eventId` invariant the client dedup relies on
/// (`acp::meta::NotificationMeta::event_seq`) spans a *session's whole history*,
/// not a single process. On `--resume` (or any reload into a fresh process) the
/// replayed transcript carries the ORIGINAL process's high counters; without
/// re-seeding, this process would mint LOWER ids for new live events and the
/// client would dedup-drop every one of them (frozen token counter, missing
/// turns). Call this once on session load with `persisted_max + 1`.
///
/// Uses `fetch_max`, so it only ever raises the counter — safe to call from
/// multiple concurrently-loading sessions sharing the process-global counter.
pub fn ensure_event_counter_at_least(next: u64) {
    EVENT_COUNTER.fetch_max(next, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant `with_event_order` exists to hold: across concurrent
    /// producers, the order events reach a shared queue is their `eventId`
    /// order. Without the lock, a thread can mint `N` and be preempted before
    /// enqueuing, letting a thread that minted `N+1` enqueue first — which
    /// writes `N+1` ahead of `N` and makes a position-resolved reconnect
    /// cursor skip the newer event permanently.
    #[test]
    fn with_event_order_keeps_queue_order_equal_to_id_order() {
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel::<u64>();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for _ in 0..250 {
                        with_event_order(|| {
                            let mut meta = None;
                            ensure_event_id_meta("sess-order", &mut meta);
                            let seq: u64 = meta.unwrap()["eventId"]
                                .as_str()
                                .unwrap()
                                .rsplit('-')
                                .next()
                                .unwrap()
                                .parse()
                                .unwrap();
                            // Stand-in for the persistence/gateway enqueues.
                            tx.send(seq).unwrap();
                        });
                    }
                })
            })
            .collect();
        drop(tx);
        for t in threads {
            t.join().unwrap();
        }

        let received: Vec<u64> = rx.iter().collect();
        assert_eq!(received.len(), 8 * 250);
        assert!(
            received.windows(2).all(|w| w[0] < w[1]),
            "queue order diverged from eventId order",
        );
    }

    #[test]
    fn test_generate_event_id_format() {
        let id = generate_event_id("test-session-123");
        assert!(id.starts_with("test-session-123-"));
        // Should end with a valid number
        let _counter: u64 = id.rsplit('-').next().unwrap().parse().unwrap();
    }

    #[test]
    fn ensure_event_counter_at_least_only_raises() {
        // Re-seeding to a high floor makes the next id continue past it — this
        // is what keeps `--resume` from minting ids below the replayed maximum.
        // Uses a very high floor so concurrent tests (which only ever raise the
        // shared counter via fetch_add/fetch_max) cannot push it back down.
        ensure_event_counter_at_least(5_000_000);
        let counter1: u64 = generate_event_id("sess")
            .rsplit('-')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            counter1 >= 5_000_000,
            "next id must be at/above the seeded floor, got {counter1}"
        );

        // A lower floor is a no-op (fetch_max never decreases the counter).
        ensure_event_counter_at_least(1);
        let counter2: u64 = generate_event_id("sess")
            .rsplit('-')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            counter2 > counter1,
            "a lower floor must not reset the counter: {counter2} !> {counter1}"
        );
    }

    #[test]
    fn ensure_event_id_meta_stamps_none_and_merges_existing() {
        // None meta: a fresh object with eventId + timestamp is created.
        let mut meta = None;
        ensure_event_id_meta("sess-x", &mut meta);
        let obj = meta.as_ref().unwrap();
        assert!(
            obj["eventId"]
                .as_str()
                .is_some_and(|id| id.starts_with("sess-x-"))
        );
        assert!(obj["agentTimestampMs"].is_i64());

        // Existing meta without eventId: fields are merged, not replaced.
        let mut meta = serde_json::json!({ "custom": true }).as_object().cloned();
        ensure_event_id_meta("sess-x", &mut meta);
        let obj = meta.as_ref().unwrap();
        assert_eq!(obj["custom"], serde_json::json!(true));
        assert!(obj.contains_key("eventId"));
    }

    #[test]
    fn ensure_event_id_meta_keeps_existing_id() {
        // An already-stamped id (e.g. emit site stamped before the persist
        // chokepoint re-checks) must survive so the persisted line matches
        // the live broadcast copy.
        let mut meta = serde_json::json!({ "eventId": "sess-x-42" })
            .as_object()
            .cloned();
        ensure_event_id_meta("sess-x", &mut meta);
        assert_eq!(
            meta.as_ref().and_then(|m| m.get("eventId")),
            Some(&serde_json::json!("sess-x-42"))
        );
    }

    #[test]
    fn test_generate_event_id_incrementing() {
        let id1 = generate_event_id("session-a");
        let id2 = generate_event_id("session-b");
        let id3 = generate_event_id("session-a");

        let counter1: u64 = id1.rsplit('-').next().unwrap().parse().unwrap();
        let counter2: u64 = id2.rsplit('-').next().unwrap().parse().unwrap();
        let counter3: u64 = id3.rsplit('-').next().unwrap().parse().unwrap();

        // Counters should be monotonically increasing
        assert!(counter2 > counter1);
        assert!(counter3 > counter2);
    }
}
