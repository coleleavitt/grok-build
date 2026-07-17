# PRD: Anthropic OAuth account rotation after Fable rate limits

## Problem
Claude/Fable requests can report the subscription limit as consumed even when multiple Anthropic OAuth accounts are stored and usable. Repository analysis found two regressions:

1. `AccountData::select()` treated missing `active_index` as an implicit sticky pin to account 0.
2. Anthropic rate-limit sampler failures surfaced terminally without marking the selected account cooled down or rotating to another account.

## Goal
When an Anthropic provider request is rate-limited, the client should stop reusing the same cached bearer, mark the selected account as rate-limited, resolve the next usable account, update sampler config, and retry through the existing resubmit path.

## Non-goals
- No new scheduler, background balancer, or account-weight system.
- No live Anthropic API calls in unit tests.
- No token or refresh-secret output in logs, docs, or TestSprite artifacts.

## Acceptance criteria

### TC001 Missing active index is not sticky account 0
Given multiple ready Anthropic accounts and `active_index = None`, selection must not treat index 0 as a manual pin. It should choose by the normal health/tier/name ordering.

### TC002 Explicit active index remains sticky
Given a manually selected account with `active_index = Some(i)` and healthy status, selection keeps that account sticky.

### TC003 Rate-limit mark rotates credentials
Given two ready accounts, when the selected account is recorded as rate-limited with a retry-after value, its `unified_status` becomes `Rejected`, `rate_limit_reset_time` is in the future, and the next resolved credential comes from the other ready account without touching the network.

### TC004 Live cache is cleared on rate limit
Given `LiveCredential` has cached a bearer, when `record_rate_limit()` marks the selected account, the cached bearer is cleared so the next sampler config cannot keep sending the exhausted bearer.

### TC005 Sampler rate-limit path retries Anthropic rotation
Given a sampler failure with `SamplingErrorKind::RateLimited` and an Anthropic provider adapter, the session should call the Anthropic live credential rotation path and return the existing resubmit recovery instead of immediately reporting exhausted. If no usable replacement remains, it may still report exhausted.

## Code summary
- `crates/codegen/xai-grok-anthropic-auth/src/store.rs`
  - `AccountData::select()` now only applies sticky active behavior when `active_index` is explicitly set.
  - Regression tests cover missing vs explicit active index behavior.
- `crates/codegen/xai-grok-anthropic-auth/src/manager.rs`
  - `record_selected_rate_limit()` marks the selected account rejected and applies `Retry-After` cooldown, with a 5-minute fallback only when no header is present.
  - Regression test proves the next credential rotates to another ready account.
- `crates/codegen/xai-grok-anthropic-auth/src/live.rs`
  - `record_rate_limit()` delegates to the manager and clears the process-local cached bearer.
  - Regression test proves cached bearer invalidation.
- `crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampler_turn.rs`
  - Anthropic rate-limit failures now mark/rotate/refresh before returning the existing resubmit recovery.
- `crates/codegen/xai-grok-shell/src/session/acp_session_impl/turn.rs`
  - Retry copy is generic for refreshed or rotated credentials.

## Test commands
- `cargo test -p xai-grok-anthropic-auth --no-default-features`
- `cargo check -p xai-grok-shell`
- `testsprite_loop(generate=true, changed=true, since=HEAD, require_approved_prd=false)`

## Official TestSprite code summary
Generated at `testsprite_tests/tmp/code_summary.yaml`.
