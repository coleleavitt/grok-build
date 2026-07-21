# TestSprite Summary — xai-grok-brain (Onyx "Brain" port)

- **Date:** 2026-07-21
- **PRD:** `brain_standard_prd.json` (stored + approved, id `9dc9982a-d063-43f5-9d0f-0119a955e5fe`; review page: `brain_prd_review.html`)
- **Scope:** the `crates/codegen/xai-grok-brain` library crate — the Rust port of Onyx's Brain self-improving memory graph (`backend/onyx/db/brain.py` + `brain/tasks.py`).
- **Modality:** `command` tests driving the crate's real in-repo cargo suite (22 cargo tests: 18 unit, 1 fresh-consumer integration, 3 Onyx-parity integration). Deterministic, no live server, no LLM.

## Requirement Validation Summary — all 7 passed (7/7, 100%)

| # | Requirement | TestSprite test | Cargo tests driven | Verdict |
|---|---|---|---|---|
| F001 | Memory page store (CRUD, derived titles, reopen persistence) | Brain F001 | `page_crud_persists_across_reopen`, `title_derived_from_first_sentence_when_absent`, `update_missing_page_is_page_not_found` | ✅ Passed |
| F002 | Relations + graph (dedup, self-edge/unknown rejection, removal, grouped related, degree-0 nodes) | Brain F002 | `relation_dedup_both_directions`, `self_edge_rejected`, `relation_to_unknown_page_rejected`, `relation_removal`, `related_pages_grouped_by_category`, `graph_includes_degree_zero_nodes` | ✅ Passed |
| F003 | Typed citations + drift-tolerant ref normalization (`[s1]`/`S1`/`[D3]`/`d4`) | Brain F003 | `source_attach_and_list`, `source_ref_normalization_tolerates_drift` | ✅ Passed |
| F004 | Settings round-trip, focus-clear semantics, run-complete stamp | Brain F004 | `settings_defaults_and_roundtrip`, `mark_run_complete_updates_timestamp` | ✅ Passed |
| F005 | Self-improvement run engine (gating, categorization, sources, links, stamp) | Brain F005 | `run_skipped_when_disabled`, `run_applies_categorized_linked_cited_pages`, `run_updates_existing_page_instead_of_duplicating`, `run_without_connectors_excludes_documents`, `empty_context_stamps_run_without_calling_provider` | ✅ Passed |
| F006 | Onyx behavioral parity + fresh-consumer public API | Brain F006 | `tests/onyx_parity.rs` (3 tests, direct port of Onyx `test_memory_recall_and_graph.py`) + `tests/consumer.rs` | ✅ Passed |
| Gate | Build compiles, clippy clean (`-D warnings`) | Brain gate | `cargo build` + `cargo clippy -- -D warnings` | ✅ Passed |

## Parity evidence

The reference Onyx suite behind the original testsprite brain script
(`047535cd..._Memory_recall_context___brain_graph__DB_layer.sh` →
`backend/tests/external_dependency_unit/tools/test_memory_recall_and_graph.py`)
was run against Onyx's real Postgres in the same session: **4 passed**. Its
scenarios are ported 1:1 (same seed data, topology, and assertions) in
`crates/codegen/xai-grok-brain/tests/onyx_parity.rs`, which passes here — same
graph (3 nodes / 2 edges / degrees 2-1-1), same citations, same guards, same
ordering, from both implementations.

## Key gaps / risks

- None for the brain crate. (The combined `brain_testsprite_report.md` also
  re-lists this repo's 30 pre-existing anthropic-auth tests, whose stale
  `routing_404` failures come from an earlier session's suite that needs a
  live server — unrelated to the brain feature.)
- Out of scope by design (plan non-goals): HTTP endpoints, UI/graph view,
  scheduling daemon, multi-user store, real LLM extraction provider.

## Artifacts

- `brain_standard_prd.json` — the PRD (ingested + persisted + approved)
- `brain_prd_review.html` — PRD/plan review page
- `brain_testsprite_report.md` — official-format requirement validation report (whole project)
- `brain_dashboard.html` — local dashboard
- `TC/…_Brain_*.sh` code files — the materialized runnable test commands
- Re-run anytime: the 7 brain tests via `testsprite_run`, or natively `cargo test -p xai-grok-brain`
