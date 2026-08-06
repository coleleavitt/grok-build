//! Chaos harness for the reconnect-replay contract.
//!
//! Applies the chaos-engineering cycle (hypothesis → experiment → analysis) to
//! the one property this whole subsystem exists to provide: a client that
//! reconnects with a cursor receives exactly the events it has not seen.
//!
//! # Why fault injection rather than more example tests
//!
//! The bugs this subsystem has actually shipped were not wrong logic on the
//! happy path. They were correct logic under an interleaving nobody wrote a
//! test for: two producers racing to append, a client disconnecting between a
//! mint and its append, a crash between two durable writes. Example-based tests
//! encode the interleavings their author imagined, which is exactly the set
//! that does not contain the bug.
//!
//! So the experiment here enumerates the fault space instead of sampling it.
//! For the small event counts where these bugs live, exhaustive enumeration is
//! both cheap and *complete* — strictly better than the random fault injection
//! the chaos-engineering literature defaults to, because a passing run is a
//! proof over that space rather than an absence of evidence.
//!
//! # The steady state (§ hypothesis)
//!
//! For every append interleaving and every cursor a client could hold:
//!
//! - **H1 no gap** — every persisted event newer than the cursor is forwarded.
//!   A violation is unrecoverable: nothing later moves the cursor back.
//! - **H2 monotone delivery** — the forwarded set ascends by counter, so the
//!   client's own seq-highwater dedup cannot drop something it was just sent.
//! - **H3 incremental preserved** — a cursor that names a line on disk resolves,
//!   rather than silently degrading to a full replay.
//!
//! The harness is validated against a deliberately broken reference
//! ([`positional_forward_set`], the pre-fix behavior) in
//! [`harness_detects_the_regression_it_was_built_for`]. A chaos harness that
//! finds nothing is indistinguishable from one that cannot find anything.

use super::prepare_replay_lines;

/// One persisted event: the counter it minted and a payload that identifies it.
#[derive(Clone, Copy, Debug)]
struct Event {
    seq: u64,
}

fn line(event: Event) -> String {
    format!(
        r#"{{"method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"e{seq}"}}}},"_meta":{{"eventId":"s-{seq}"}}}}}}"#,
        seq = event.seq
    )
}

/// Assemble `updates.jsonl` for one append interleaving.
///
/// `append_order` indexes into `events`, so it expresses "which minted event
/// reached the log first" — the single degree of freedom that produced every
/// ordering bug this subsystem has had.
fn transcript(events: &[Event], append_order: &[usize]) -> String {
    let mut out = String::new();
    for &index in append_order {
        out.push_str(&line(events[index]));
        out.push('\n');
    }
    out
}

/// Every ordering of `n` appends. `n` stays small on purpose: 5! = 120 is
/// exhaustive and instant, and no ordering bug in this subsystem has ever
/// needed more than three concurrent producers to reproduce.
fn permutations(n: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut current: Vec<usize> = (0..n).collect();
    permute(&mut current, 0, &mut out);
    out
}

fn permute(current: &mut Vec<usize>, start: usize, out: &mut Vec<Vec<usize>>) {
    if start == current.len() {
        out.push(current.clone());
        return;
    }
    for index in start..current.len() {
        current.swap(start, index);
        permute(current, start + 1, out);
        current.swap(start, index);
    }
}

/// The counters the production implementation would forward for `cursor`.
fn forwarded_seqs(transcript: &str, cursor: Option<&str>) -> (Vec<u64>, bool) {
    let prepared = prepare_replay_lines(transcript, cursor);
    let seqs = prepared
        .lines
        .iter()
        .filter_map(|line| super::line_event_seq(line))
        .collect();
    (seqs, prepared.mark_replay)
}

/// The pre-fix selection rule, kept as a control: take the file-position tail.
///
/// Not dead code — [`harness_detects_the_regression_it_was_built_for`] runs the
/// same experiment against it and requires a violation, which is what proves
/// the harness can see this class of fault at all.
fn positional_forward_set(events: &[Event], append_order: &[usize], cursor: u64) -> Vec<u64> {
    let appended: Vec<u64> = append_order.iter().map(|&i| events[i].seq).collect();
    match appended.iter().position(|&seq| seq == cursor) {
        Some(index) => appended[index + 1..].to_vec(),
        None => appended,
    }
}

/// H1: no event newer than the cursor may be missing from the forwarded set.
fn check_no_gap(appended: &[u64], cursor: u64, forwarded: &[u64]) -> Result<(), String> {
    let expected: Vec<u64> = appended
        .iter()
        .copied()
        .filter(|&seq| seq > cursor)
        .collect();
    for seq in &expected {
        if !forwarded.contains(seq) {
            return Err(format!(
                "H1 violated: event {seq} is newer than cursor {cursor} but was not forwarded \
                 (on-disk order {appended:?}, forwarded {forwarded:?})"
            ));
        }
    }
    Ok(())
}

/// H2: the forwarded set must ascend, or the client's dedup drops the tail.
fn check_monotone(forwarded: &[u64]) -> Result<(), String> {
    if forwarded.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(format!(
            "H2 violated: forwarded set is not ascending: {forwarded:?}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(count: u64) -> Vec<Event> {
        // Counters are minted from a shared global, so they are distinct and
        // ascending regardless of who mints them.
        (0..count).map(|seq| Event { seq: seq + 1 }).collect()
    }

    /// The full experiment: every append interleaving crossed with every cursor
    /// a client could be holding.
    #[test]
    fn every_interleaving_and_cursor_upholds_the_replay_contract() {
        let events = events(5);
        let mut schedules = 0usize;

        for append_order in permutations(events.len()) {
            let transcript = transcript(&events, &append_order);
            let appended: Vec<u64> = append_order.iter().map(|&i| events[i].seq).collect();

            for &cursor in &appended {
                let cursor_id = format!("s-{cursor}");
                let (forwarded, mark_replay) = forwarded_seqs(&transcript, Some(&cursor_id));

                assert!(
                    !mark_replay,
                    "H3 violated: cursor s-{cursor} names a line on disk but replay was \
                     downgraded to full (on-disk order {appended:?})",
                );
                check_no_gap(&appended, cursor, &forwarded).unwrap();
                check_monotone(&forwarded).unwrap();
                schedules += 1;
            }
        }

        assert_eq!(
            schedules,
            120 * 5,
            "the experiment must actually cover 5! orderings x 5 cursors",
        );
    }

    /// A harness that cannot fail proves nothing. Run the identical experiment
    /// against the pre-fix positional rule and require it to break — this is
    /// the regression that shipped, reproduced mechanically.
    #[test]
    fn harness_detects_the_regression_it_was_built_for() {
        let events = events(4);
        let mut violations = Vec::new();

        for append_order in permutations(events.len()) {
            let appended: Vec<u64> = append_order.iter().map(|&i| events[i].seq).collect();
            for &cursor in &appended {
                let forwarded = positional_forward_set(&events, &append_order, cursor);
                if let Err(reason) = check_no_gap(&appended, cursor, &forwarded) {
                    violations.push(reason);
                }
            }
        }

        assert!(
            !violations.is_empty(),
            "the positional rule must violate H1 somewhere, or the harness is blind",
        );
        // Every violation is a permanently lost event, which is why the fix was
        // not optional.
        assert!(
            violations.iter().any(|v| v.contains("was not forwarded")),
            "expected a lost-event violation, got: {violations:?}",
        );
    }

    /// Fault: the client disconnects holding a cursor for an event that was
    /// minted but never appended (the process died in between). The cursor
    /// names nothing on disk, so the only safe answer is a full replay — a
    /// partial tail would silently skip whatever landed before it.
    #[test]
    fn a_cursor_for_a_never_appended_event_falls_back_to_full_replay() {
        let events = events(3);
        for append_order in permutations(events.len()) {
            let transcript = transcript(&events, &append_order);
            let (forwarded, mark_replay) = forwarded_seqs(&transcript, Some("s-99"));
            assert!(mark_replay, "an unresolvable cursor must force full replay");
            assert_eq!(
                forwarded.len(),
                events.len(),
                "a full replay forwards everything on disk",
            );
        }
    }

    /// Fault: a crash truncates the log after the cursor. Whatever survived
    /// must still be delivered in counter order, with no gap below the tail.
    #[test]
    fn a_truncated_log_still_delivers_its_surviving_tail() {
        let events = events(5);
        for append_order in permutations(events.len()) {
            let appended: Vec<u64> = append_order.iter().map(|&i| events[i].seq).collect();
            for truncate_at in 1..=appended.len() {
                let survivors = &append_order[..truncate_at];
                let transcript = transcript(&events, survivors);
                let on_disk: Vec<u64> = survivors.iter().map(|&i| events[i].seq).collect();

                for &cursor in &on_disk {
                    let cursor_id = format!("s-{cursor}");
                    let (forwarded, _) = forwarded_seqs(&transcript, Some(&cursor_id));
                    check_no_gap(&on_disk, cursor, &forwarded).unwrap();
                    check_monotone(&forwarded).unwrap();
                }
            }
        }
    }

    /// Reconnecting twice in a row must converge: the second reconnect, using
    /// the cursor the first one ended on, has nothing left to deliver.
    /// Without this an idle client re-receives the same tail forever.
    #[test]
    fn a_second_reconnect_from_the_delivered_tail_is_empty() {
        let events = events(4);
        for append_order in permutations(events.len()) {
            let transcript = transcript(&events, &append_order);
            let appended: Vec<u64> = append_order.iter().map(|&i| events[i].seq).collect();

            for &cursor in &appended {
                let first = format!("s-{cursor}");
                let (forwarded, _) = forwarded_seqs(&transcript, Some(&first));
                let Some(&last) = forwarded.last() else {
                    continue;
                };
                let second = format!("s-{last}");
                let (again, mark_replay) = forwarded_seqs(&transcript, Some(&second));
                assert!(!mark_replay);
                assert!(
                    again.is_empty(),
                    "reconnecting from the delivered tail re-sent {again:?}",
                );
            }
        }
    }
}
