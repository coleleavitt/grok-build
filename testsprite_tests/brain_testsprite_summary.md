# TestSprite Summary — xai-grok-brain + Grok Build request-path wiring

- **Date:** 2026-07-21
- **Current PRD:** `brain_standard_prd.json` (stored + approved, id `7f241280-3f43-4ccb-a6dd-c8e3b8fecdf3`; review page: `brain_prd_review.html`)
- **Scope:** `crates/codegen/xai-grok-brain` plus the `xai-grok-shell` prompt-path integration seam that calls it from normal user-originated turns.
- **Modality:** deterministic TestSprite `command` tests driving real cargo tests. No live server, no network LLM; the shell tests drive the same `process_brain_request_at_path` helper that `handle_prompt` calls before request construction, and verify ChatStateActor request injection.

## Requirement Validation Summary — Brain group all passed (10/10, 100%)

| # | Requirement | TestSprite test | Real path exercised | Verdict |
|---|---|---|---|---|
| F001 | Memory page store (CRUD, derived titles, reopen persistence) | Brain F001 | `xai-grok-brain` store tests | ✅ Passed |
| F002 | Relations + graph (dedup, self-edge/unknown rejection, removal, grouped related, degree-0 nodes) | Brain F002 | `xai-grok-brain` graph tests | ✅ Passed |
| F003 | Typed citations + drift-tolerant ref normalization (`[s1]`/`S1`/`[D3]`/`d4`) | Brain F003 | `xai-grok-brain` source/ref tests | ✅ Passed |
| F004 | Settings round-trip, focus-clear semantics, run-complete stamp | Brain F004 | `xai-grok-brain` settings tests | ✅ Passed |
| F005 | Self-improvement run engine (gating, categorization, sources, links, stamp) | Brain F005 | `run_self_improvement` pipeline tests | ✅ Passed |
| F006 | Onyx DB-layer behavioral parity + fresh-consumer public API | Brain F006 | `tests/onyx_parity.rs` + `tests/consumer.rs` | ✅ Passed |
| F007 | Portable live-demo lifecycle parity (populate all categories, counts, graph, engine refresh/recall analog, cleanup) | Brain F007 | `tests/live_demo_parity.rs` + category-count tests | ✅ Passed |
| F008 | Grok Build request-path Brain wiring | Brain F008 | `xai-grok-shell` helper used by `handle_prompt`: remember request → durable page/source; reopen → fresh request recall; ChatStateActor `build_request` injection | ✅ Passed |
| F009 | Integration settings gate + deterministic self-improvement | Brain F009 | shell disabled-setting integration + `BrainService` deterministic-provider run | ✅ Passed |
| Gate | Build/clippy gate | Brain gate | `cargo build`/`cargo clippy` for Brain crate command test | ✅ Passed |

## Wired behavior now proven

- **Create-on-request:** a normal user-originated prompt like “Please remember that my project codename is Zephyr-Shell” is processed by the shell Brain seam before model sampling. It creates a durable Brain page (`Project Codename`, `entities`) and a `chat_session` source pointing to the Grok session/prompt.
- **Recall-on-later-request:** a later fresh request against a reopened store gets a `<brain_context>` block containing the stored fact. The shell test verifies this block enters the shipped `ChatStateActor::build_request` memory-reminder injection path.
- **Settings gate:** an explicit disabled setting blocks both create and recall, even when the shell helper is called with normal initialization.
- **Self-improvement:** `BrainService::run_self_improvement` accepts a deterministic provider in tests, creates/updates categorized pages, attaches normalized session/document sources, links related pages, and stamps `last_run_at`.

## Parity with Onyx TestSprite artifacts

Onyx’s portable Brain/memory tests are represented as follows:

| Onyx artifact | Port status |
|---|---|
| `047535cd...Memory_recall_context___brain_graph__DB_layer.sh` | ✅ `tests/onyx_parity.rs` |
| `6ef8253f...Memory_live_demo__populate___recall_on_running_stack.sh` | ✅ portable parts in `tests/live_demo_parity.rs` |
| `944ae2c2...Memory_populate___recall__integration__real_stack.sh` | ✅ list/count/filter and deterministic engine analog covered; ⛔ live LLM chat recall remains out-of-scope for tests |
| `a4235a99...Memory_UI_lifecycle__Playwright.sh` | ⛔ out-of-scope (no Brain UI in this crate/goal) |

## Verification evidence

Scratch logs for the goal are under `/tmp/grok-goal-9d924f6d31fa/implementer/`:

- `test.log` — `cargo test -p xai-grok-brain` + `cargo test -p xai-grok-shell brain::tests`
- `launch.log` — shell launch/request-path check (create/reopen/recall + ChatState injection)
- `settings_engine.log` — disabled setting + deterministic-provider self-improvement checks
- `testsprite.log` — official JSON report plus appended Brain-only 10/10 MCP run summary
- `build_clippy.log` — `cargo build -p xai-grok-brain -p xai-grok-shell` succeeded; `cargo clippy` completed with pre-existing shell warnings unrelated to Brain wiring (documented in the log)

## Remaining explicit non-goals

- No web UI or force-directed graph visualization.
- No real network LLM extraction in tests; the provider boundary is production-ready and deterministic in tests.
- No scheduler daemon; callers invoke the self-improvement service entry point.
- No multi-user authorization layer beyond the local/session model.
