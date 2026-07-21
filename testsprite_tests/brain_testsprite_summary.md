# TestSprite Summary — xai-grok-brain (Onyx "Brain" port)

- **Date:** 2026-07-21
- **Current PRD:** `brain_standard_prd.json` (stored + approved, id `38a75df9-18f0-46bf-84f2-f04dc39aed44`; review page: `brain_prd_review.html`)
- **Scope:** `crates/codegen/xai-grok-brain`, the Rust library port of Onyx's Brain/memory domain (`backend/onyx/db/brain.py` + `brain/tasks.py`) plus the portable parts of Onyx's memory testsprite suite.
- **Modality:** deterministic TestSprite `command` tests driving the crate's real cargo suite. No live server, no UI, no network LLM.

## Requirement Validation Summary — all 8 passed (8/8, 100%)

| # | Requirement | TestSprite test | Cargo tests driven | Verdict |
|---|---|---|---|---|
| F001 | Memory page store (CRUD, derived titles, reopen persistence) | Brain F001 | `page_crud_persists_across_reopen`, `title_derived_from_first_sentence_when_absent`, `update_missing_page_is_page_not_found` | ✅ Passed |
| F002 | Relations + graph (dedup, self-edge/unknown rejection, removal, grouped related, degree-0 nodes) | Brain F002 | `relation_dedup_both_directions`, `self_edge_rejected`, `relation_to_unknown_page_rejected`, `relation_removal`, `related_pages_grouped_by_category`, `graph_includes_degree_zero_nodes` | ✅ Passed |
| F003 | Typed citations + drift-tolerant ref normalization (`[s1]`/`S1`/`[D3]`/`d4`) | Brain F003 | `source_attach_and_list`, `source_ref_normalization_tolerates_drift` | ✅ Passed |
| F004 | Settings round-trip, focus-clear semantics, run-complete stamp | Brain F004 | `settings_defaults_and_roundtrip`, `mark_run_complete_updates_timestamp` | ✅ Passed |
| F005 | Self-improvement run engine (gating, categorization, sources, links, stamp) | Brain F005 | `run_skipped_when_disabled`, `run_applies_categorized_linked_cited_pages`, `run_updates_existing_page_instead_of_duplicating`, `run_without_connectors_excludes_documents`, `empty_context_stamps_run_without_calling_provider` | ✅ Passed |
| F006 | Onyx DB-layer behavioral parity + fresh-consumer public API | Brain F006 | `tests/onyx_parity.rs` (direct port of Onyx `test_memory_recall_and_graph.py`) + `tests/consumer.rs` | ✅ Passed |
| F007 | Portable live-demo lifecycle parity (populate all categories, counts, graph, engine refresh/recall analog, cleanup) | Brain F007 | `list_category_counts_and_category_filter_match_onyx_memory_list_shape` + `tests/live_demo_parity.rs` | ✅ Passed |
| Gate | Build compiles, clippy clean (`-D warnings`) | Brain gate | `cargo build` + `cargo clippy -- -D warnings` | ✅ Passed |

## What was missing from the first pass and is now covered

Onyx had four memory/brain TestSprite tests:

| Onyx TestSprite test | Onyx behavior | Port status |
|---|---|---|
| `047535cd...Memory_recall_context___brain_graph__DB_layer.sh` | DB-layer brain graph, sources, recall context ordering | ✅ `tests/onyx_parity.rs` |
| `6ef8253f...Memory_live_demo__populate___recall_on_running_stack.sh` | Populate all categories, list totals/counts, graph, chat recall, cleanup | ✅ portable parts in `tests/live_demo_parity.rs` (chat recall replaced by deterministic engine refresh/recall analog because this crate has no chat server) |
| `944ae2c2...Memory_populate___recall__integration__real_stack.sh` | Manual populate + category counts + category filter; real LLM chat recall; memory tool persistence | ✅ portable list/count/filter part added to `BrainStore`; ✅ engine provider covers memory-tool persistence analog; ⛔ real LLM chat recall is out-of-scope for a local library crate |
| `a4235a99...Memory_UI_lifecycle__Playwright.sh` | Add/reload/edit/delete through Onyx web UI | ⛔ out-of-scope (the crate has no UI; original plan explicitly excluded frontend) |

## Parity evidence

- The original Onyx DB-layer suite behind `047535cd...` was run against Onyx's real Postgres in this session: **4 passed**.
- The crate now has three parity integration files:
  - `tests/onyx_parity.rs` — direct port of Onyx graph/source/recency assertions.
  - `tests/consumer.rs` — fresh public API consumer: two pages, one relation, one source, graph return value asserted.
  - `tests/live_demo_parity.rs` — portable version of Onyx `memory_demo_populate.py`: all categories seeded, category counts checked, graph checked before/after engine linking, provider input/focus checked, session citation attached, cleanup verified empty.

## Remaining non-gaps / explicit non-goals

- No HTTP endpoints, FastAPI routes, or frontend Playwright flow in this crate.
- No scheduled daemon/celery parity; callers invoke `run_self_improvement` directly.
- No live LLM provider; `ExtractionProvider` is intentionally pluggable and deterministic tests prove the real pipeline around it.
- No multi-user ownership model; invalid endpoint/self-edge guards cover the local single-user store's equivalent invariant.

## Artifacts

- `brain_standard_prd.json` — PRD (ingested + approved)
- `brain_prd_review.html` — PRD/plan review
- `brain_testsprite_report.md` — official TestSprite report (whole project DB; includes older unrelated anthropic-auth tests)
- `brain_dashboard.html` — dashboard
- `brain_testsprite_summary.md` — this scoped summary
- `0c04e6c1...Brain_F007...sh` plus F001–F006/gate `.sh` files — materialized runnable TestSprite command tests
