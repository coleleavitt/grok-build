# Onyx Brain → Grok Build Brain parity matrix

This file maps the portable Onyx Python Brain behavior to the shipped Rust entry
points and tests. Non-portable infrastructure (FastAPI, Postgres, Celery, web UI)
is intentionally represented by local/service equivalents.

| Onyx Python behavior | Rust shipped entry point | Proof |
|---|---|---|
| `BrainSettings.from_user`, `update_brain_settings`, `mark_brain_run_complete` | `BrainStore::settings`, `update_settings`, `mark_run_complete`; `BrainService` settings methods | `settings_defaults_and_roundtrip`, `mark_run_complete_updates_timestamp`, `disabled_settings_do_not_create_or_recall` |
| `MemoryCategory` = notes/concepts/entities/workstreams | `MemoryCategory` enum with stable string values | `list_category_counts_and_category_filter_match_onyx_memory_list_shape`, self-improvement tests |
| Page CRUD/list/category filtering/counts (`/memory`) | `BrainStore::{create_page,get_page,list_pages,list_pages_by_category,category_counts,update_page,delete_page}` | CRUD tests, live-demo parity test |
| `MemoryRelation` ordered undirected edges; self-edge/user-ownership rejection; remove is idempotent | `BrainStore::{add_relation,remove_relation,related_page_ids,related_pages,graph}`; local single-user invalid endpoint replaces cross-user ownership | `relation_dedup_both_directions`, `self_edge_rejected`, `relation_to_unknown_page_rejected`, `relation_removal`, `onyx_parity.rs` |
| `MemorySource` citations; label cap; source listing order | `BrainStore::{add_source,add_source_if_missing,sources}` | `source_attach_and_list`, request-path shell tests, backfill launch test |
| `_normalize_source_ref` handles `[s1]`, `S1`, `[D3]`, `d4` | `engine::normalize_source_ref` | `source_ref_normalization_tolerates_drift`, engine/backfill tests |
| `_recent_sessions`: max 25 recent sessions, cutoff = max(now-14d, last_run_at) | `backfill::build_bounded_run_context`, `read_bounded_run_context` | `bounded_context_applies_recent_window_last_run_and_caps` |
| `_build_context`: `[S#]` chat refs, chronological user/assistant lines, max 20 msgs/session, 800 chars/message, 24k chars total | `backfill::build_bounded_run_context` + `engine` transcript builder | `bounded_context_applies_recent_window_last_run_and_caps`, `backfill_launch.rs` provider-input assertions |
| `_collect_cited_documents`: connector/document refs only when enabled, max 20 docs | `read_persisted_sessions` discovers file/document-like refs; `build_bounded_run_context` includes docs only when `settings.use_connectors` | `connector_toggle_controls_documents`, `reads_jsonl_session_and_discovers_file_sources`, `backfill_launch.rs` |
| `_extract_pages`: pluggable LLM parses categorized pages with existing titles/focus/max pages | `ExtractionProvider` trait and `ExtractionInput` (`focus_instructions`, `existing_titles`, `max_pages`) | `run_applies_categorized_linked_cited_pages`, `service_self_improvement_uses_deterministic_provider`, `backfill_launch.rs` |
| `_apply_pages`: create-or-update by normalized title, unknown category -> notes, attach normalized sources, dedup existing sources, then link related pages | `engine::run_self_improvement`; request path uses `create_or_update_page_by_title` and source dedup | engine tests, `remember_request_creates_record_source_and_later_context_after_reopen`, `backfill_launch.rs` repeated-run assertion |
| `_run_for_user`: no sessions/transcript still stamps last_run; disabled user skips task | `BrainService::run_backfill_from_grok_home(_at)` + `run_self_improvement` | `empty_context_stamps_run_without_calling_provider`, disabled service tests, bounded backfill tests |
| Celery `brain_self_improvement_user` / `POST /memory/brain/run` on-demand run | `BrainService::run_backfill_from_grok_home` callable service boundary; shell can expose commands later | `backfill_launch.rs`, TestSprite F009 |
| Daily Celery beat at 03:30 | Not literally ported; local Grok Build has no Celery. Portable behavior is the callable bounded backfill entry point. | Non-goal in plan |
| FastAPI graph/settings/source endpoints and React graph UI | Not literally ported; local crate/service exposes graph/settings/source methods. | Non-goal in plan; store/service tests |
