# TestSprite Summary — full Onyx Brain Python logic port

- **Date:** 2026-07-21
- **Current PRD:** `brain_standard_prd.json` (stored + approved, id `757028a7-b5be-4e09-84ab-f253175df1ba`; review page: `brain_prd_review.html`)
- **Scope:** `xai-grok-brain` plus the `xai-grok-shell` prompt-path seam. This covers the portable behavior of Onyx `backend/onyx/db/brain.py` and `backend/onyx/background/celery/tasks/brain/tasks.py` in Grok Build’s local/session model.
- **Modality:** deterministic TestSprite `command` tests driving real cargo tests. No network LLM; provider behavior is injected through the shipped `ExtractionProvider` trait.

## Requirement Validation Summary — Brain group all passed (11/11, 100%)

| # | Requirement | TestSprite test | Real path exercised | Verdict |
|---|---|---|---|---|
| F001 | Page CRUD/reopen persistence | Brain F001 | `BrainStore` CRUD/reopen tests | ✅ Passed |
| F002 | Relations + graph | Brain F002 | `BrainStore` relation/graph tests | ✅ Passed |
| F003 | Source citations + ref normalization | Brain F003 | `BrainStore` source tests + `normalize_source_ref` | ✅ Passed |
| F004 | Settings + last-run stamp | Brain F004 | settings roundtrip/run-complete tests | ✅ Passed |
| F005 | Self-improvement engine | Brain F005 | `run_self_improvement` deterministic provider tests | ✅ Passed |
| F006 | Onyx DB-layer parity + consumer API | Brain F006 | direct Onyx parity port + `tests/consumer.rs` | ✅ Passed |
| F007 | Portable live-demo lifecycle | Brain F007 | all-category populate/counts/graph/run/cleanup | ✅ Passed |
| F008 | Request create/recall path | Brain F008 | `BrainService::process_request` create/reopen/recall + update-not-duplicate/source dedup | ✅ Passed |
| F009 | Settings gate + deterministic service engine | Brain F009 | disabled create/recall + service self-improvement | ✅ Passed |
| F010 | Full Onyx Python run/backfill parity | Brain F010 | persisted Grok JSONL session reader, bounded backfill, `[S#]`/`[D#]` source maps, connector toggle, focus, source drift, graph/source/recall assertions | ✅ Passed |
| Gate | Build/clippy clean for Brain crate | Brain gate | `cargo build -p xai-grok-brain && cargo clippy -p xai-grok-brain -- -D warnings` | ✅ Passed |

## Full Python logic now ported (portable behavior)

The committed `crates/codegen/xai-grok-brain/PARITY.md` maps Onyx Python functions to Rust entry points/tests. Highlights:

- `brain.py` data layer → `BrainStore` settings, CRUD/list/filter/counts, relations, sources, graph.
- `_recent_sessions` → `build_bounded_run_context`: max 25 sessions, cutoff = max(now - 14 days, `last_run_at`).
- `_build_context` → persisted Grok JSONL reader + context builder: `[S#]` chat refs, chronological real user/assistant lines, max 20 messages/session, max 800 chars/message, max 24k transcript chars.
- `_collect_cited_documents` → file/document-like source discovery from persisted messages, included only when `use_connectors` is true, capped at 20 docs.
- `_extract_pages` → `ExtractionProvider` with `ExtractionInput` carrying transcript, existing titles, focus instructions, and max pages.
- `_apply_pages` → create/update by normalized title, unknown category fallback, source ref normalization, source dedup, related linking.
- `_run_for_user` / on-demand run → `BrainService::run_backfill_from_grok_home(_at)` and `run_self_improvement`.

## What remains explicitly non-portable / non-goal

- Literal FastAPI/Postgres/Celery infrastructure.
- Onyx React UI / force-directed graph visualization.
- Real network LLM calls in tests.
- Unlimited historical scan; behavior matches Onyx’s bounded recent-session/last-run semantics.

## Scratch verification files

For the active goal, logs are under `/tmp/grok-goal-3df1a5ee0355/implementer/` after final verification:

- `test.log` — native Brain + shell integration tests.
- `launch.log` — public backfill launch check + request recall.
- `parity.log` — adversarial bounds/source/ref/dedup parity tests.
- `testsprite.log` — Brain group 11/11 passed summary.
- `build.log` / `clippy.log` — build and clippy for touched crates.
