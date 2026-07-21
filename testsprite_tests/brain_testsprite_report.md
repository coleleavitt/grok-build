# TestSprite AI Testing Report (MCP)

---

## 1️⃣ Document Metadata
- **Project Name:** Local Project
- **Total Tests Executed:** 38
- **Pass Rate:** 26.32%

---

## 2️⃣ Requirement Validation Summary

### Requirement R001: Ungrouped

#### Test TC001
- **Test Name:** Brain F007: portable live-demo lifecycle parity (populate, counts, graph, run, cleanup)
- **Test Code:** [Brain_F007__portable_live_demo_lifecycle_parity__populate__counts__graph__run__cleanup.py](./Brain_F007__portable_live_demo_lifecycle_parity__populate__counts__graph__run__cleanup.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

#### Test TC002
- **Test Name:** Brain F006: Onyx parity suite + fresh-consumer public API check
- **Test Code:** [Brain_F006__Onyx_parity_suite___fresh_consumer_public_API_check.py](./Brain_F006__Onyx_parity_suite___fresh_consumer_public_API_check.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

#### Test TC003
- **Test Name:** Brain F001: page CRUD + persistence across store reopen
- **Test Code:** [Brain_F001__page_CRUD___persistence_across_store_reopen.py](./Brain_F001__page_CRUD___persistence_across_store_reopen.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

#### Test TC004
- **Test Name:** Brain F004: settings round-trip, focus clear semantics, run-complete stamp
- **Test Code:** [Brain_F004__settings_round_trip__focus_clear_semantics__run_complete_stamp.py](./Brain_F004__settings_round_trip__focus_clear_semantics__run_complete_stamp.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

#### Test TC005
- **Test Name:** Brain F002: relation dedup, self-edge rejection, grouped related, graph degrees
- **Test Code:** [Brain_F002__relation_dedup__self_edge_rejection__grouped_related__graph_degrees.py](./Brain_F002__relation_dedup__self_edge_rejection__grouped_related__graph_degrees.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

#### Test TC006
- **Test Name:** Brain gate: build + clippy clean
- **Test Code:** [Brain_gate__build___clippy_clean.py](./Brain_gate__build___clippy_clean.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

#### Test TC007
- **Test Name:** Brain F003: typed source citations + drift-tolerant ref normalization
- **Test Code:** [Brain_F003__typed_source_citations___drift_tolerant_ref_normalization.py](./Brain_F003__typed_source_citations___drift_tolerant_ref_normalization.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

#### Test TC008
- **Test Name:** ensure_fresh caches credential when mutex lock succeeds and still returns credential when lock is poisoned
- **Test Code:** [ensure_fresh_caches_credential_when_mutex_lock_succeeds_and_still_returns_credential_when_lock_is_poisoned.py](./ensure_fresh_caches_credential_when_mutex_lock_succeeds_and_still_returns_credential_when_lock_is_poisoned.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_6b86aa80-9034-4015-928b-b921215c6189.py", line 49, in <module>     test_bnd001_ensure_fresh_cache_and_poisoned_mutex_behavior()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_6b86aa80-9034-4015-928b-b921215c6189.py", line 13, in test_bnd001_ensure_fresh_cache_and_poisoned_mutex_behavior     assert resp_a.status_code == 200, f"Subcase A expected 200, got {resp_a.status_code}: {resp_a.text}"            ^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Subcase A expected 200, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test posts to /tests/run on http://127.0.0.1:8080 but that endpoint is not available in the running service, returning HTTP 404 before any case logic executes. Recommended fix: Start/configure the correct test runner service (or BASE_URL/path) so POST /tests/run resolves to a valid handler, e.g., point BASE_URL to the API instance that exposes /tests/run.
---

#### Test TC009
- **Test Name:** record_rate_limit clears cache only when manager marks an account and cache lock succeeds
- **Test Code:** [record_rate_limit_clears_cache_only_when_manager_marks_an_account_and_cache_lock_succeeds.py](./record_rate_limit_clears_cache_only_when_manager_marks_an_account_and_cache_lock_succeeds.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_1dd73822-f19b-480e-b3c8-dd28484e90ce.py", line 92, in <module>     test_bnd002_record_rate_limit_boundary_and_cache_clear_behavior()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_1dd73822-f19b-480e-b3c8-dd28484e90ce.py", line 52, in test_bnd002_record_rate_limit_boundary_and_cache_clear_behavior     _set_test_mode(mode="live_injectable", manager_result="acct", poison_lock=False)     ~~~~~~~~~~~~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^   File "/tmp/ts_rs_1dd73822-f19b-480e-b3c8-dd28484e90ce.py", line 31, in _set_test_mode     assert resp.status_code == 200, f"/test/setup expected 200, got {resp.status_code}, body={resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: /test/setup expected 200, got 404, body={"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test depends on a non-production test harness endpoint (`POST /test/setup`) that is not available in the running service, causing a 404 before business logic is exercised. Recommended fix: Run against a build/profile that exposes the test-only routes (`/test/setup`, `/test/cache`) or add those endpoints to the test server configuration before executing this case.
---

#### Test TC010
- **Test Name:** store_path_exists reflects filesystem state from env-configured store path
- **Test Code:** [store_path_exists_reflects_filesystem_state_from_env_configured_store_path.py](./store_path_exists_reflects_filesystem_state_from_env_configured_store_path.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_8c3f7622-7da1-42ee-84cd-781623954a7f.py", line 67, in <module>     test_bnd003_store_path_exists_reflects_filesystem_state()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_8c3f7622-7da1-42ee-84cd-781623954a7f.py", line 34, in test_bnd003_store_path_exists_reflects_filesystem_state     assert cfg_resp.status_code in (200, 204), f"Env setup failed: {cfg_resp.status_code} {cfg_resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Env setup failed: 404 {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test assumes a non-existent or unsupported test-only endpoint (`POST /test/env`) for setting service environment variables, so setup fails before exercising the feature. Recommended fix: Update the test harness to configure env vars via the actual supported mechanism (e.g., process env before service startup) or call the correct config endpoint instead of `/test/env`.
---

#### Test TC011
- **Test Name:** resolve_headers maps resolved credential into header mutation
- **Test Code:** [resolve_headers_maps_resolved_credential_into_header_mutation.py](./resolve_headers_maps_resolved_credential_into_header_mutation.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_175ac8e1-76f7-4a25-b660-814d58ac1ec0.py", line 73, in <module>     test_bnd004_resolve_headers_maps_credential_and_propagates_error()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_175ac8e1-76f7-4a25-b660-814d58ac1ec0.py", line 23, in test_bnd004_resolve_headers_maps_credential_and_propagates_error     assert r.status_code == 200, f"Expected 200, got {r.status_code}: {r.text}"            ^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test posts to /test/resolve_headers on localhost:8080, but that endpoint is not available in the running service (returns 404 Not Found). Recommended fix: Start/configure the correct test harness service or route so POST /test/resolve_headers exists (or update BASE_URL/path to the deployed endpoint that implements this scenario).
---

#### Test TC012
- **Test Name:** record_selected_rate_limit marks selected account when found and handles missing account/selection
- **Test Code:** [record_selected_rate_limit_marks_selected_account_when_found_and_handles_missing_account_selection.py](./record_selected_rate_limit_marks_selected_account_when_found_and_handles_missing_account_selection.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_fb4f2bca-aa5e-43dc-ae77-b24cc5767140.py", line 142, in <module>     test_bnd005_record_selected_rate_limit_boundary()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_fb4f2bca-aa5e-43dc-ae77-b24cc5767140.py", line 79, in test_bnd005_record_selected_rate_limit_boundary     _upsert_account(existing, unified_status="Ok")     ~~~~~~~~~~~~~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^   File "/tmp/ts_rs_fb4f2bca-aa5e-43dc-ae77-b24cc5767140.py", line 26, in _upsert_account     assert resp.status_code in (200, 201), f"Upsert account failed: {resp.status_code} {resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Upsert account failed: 404 {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test hit a missing or misrouted API endpoint because POST /accounts returned 404 before exercising any boundary logic. Recommended fix: Start/configure the correct service version and base route so POST /accounts is registered (or update BASE_URL/path prefix to the deployed API, e.g., include /api if required).
---

#### Test TC013
- **Test Name:** disabled_reason returns oauth_error code when present, otherwise permanent_oauth_error
- **Test Code:** [disabled_reason_returns_oauth_error_code_when_present__otherwise_permanent_oauth_error.py](./disabled_reason_returns_oauth_error_code_when_present__otherwise_permanent_oauth_error.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_eae197f5-31b1-444e-a3bf-d7eca795982e.py", line 57, in <module>     test_bnd006_disabled_reason_variants()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_eae197f5-31b1-444e-a3bf-d7eca795982e.py", line 12, in test_bnd006_disabled_reason_variants     assert r_a1.status_code == 200, f"A1 expected 200, got {r_a1.status_code}: {r_a1.text}"            ^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: A1 expected 200, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test hits /anthropic_auth_error/disabled_reason on localhost:8080, but that route is not available in the running service (returns 404 before any variant logic executes). Recommended fix: Start the correct API service/version that exposes POST /anthropic_auth_error/disabled_reason (or point base_url/path to the actual mounted endpoint) before running the test.
---

#### Test TC014
- **Test Name:** set_file_private is a no-op and does not fail for diverse paths
- **Test Code:** [set_file_private_is_a_no_op_and_does_not_fail_for_diverse_paths.py](./set_file_private_is_a_no_op_and_does_not_fail_for_diverse_paths.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_db17bffd-d2d2-4519-b137-2234177006de.py", line 65, in <module>     test_bnd007_set_file_private_boundary_paths()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_db17bffd-d2d2-4519-b137-2234177006de.py", line 41, in test_bnd007_set_file_private_boundary_paths     assert successful_endpoint is not None, f"Could not locate set_file_private endpoint. Last error: {last_error}"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Could not locate set_file_private endpoint. Last error: /v1/set_file_private POST -> 404, body={"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test hard-codes a small set of endpoint paths and fails when the API exposes `set_file_private` at a different route, producing 404s despite potentially correct functionality. Recommended fix: Update the test to discover the real route from the service OpenAPI/route listing (or configure the endpoint via test input) instead of guessing fixed path candidates.
---

#### Test TC015
- **Test Name:** rotate_anthropic_after_rate_limit returns true only on anthropic + successful mark + usable accounts + fresh credential
- **Test Code:** [rotate_anthropic_after_rate_limit_returns_true_only_on_anthropic___successful_mark___usable_accounts___fresh_credential.py](./rotate_anthropic_after_rate_limit_returns_true_only_on_anthropic___successful_mark___usable_accounts___fresh_credential.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_7331dff4-cb15-4864-96c1-3fe3b6384cf6.py", line 170, in <module>     test_bnd008_rotate_anthropic_after_rate_limit()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_7331dff4-cb15-4864-96c1-3fe3b6384cf6.py", line 143, in test_bnd008_rotate_anthropic_after_rate_limit     assert selected_path is not None, "No compatible harness endpoint found (all candidates returned 404)."            ^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: No compatible harness endpoint found (all candidates returned 404).
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test failed before exercising logic because the expected local harness route for rotate_anthropic_after_rate_limit was not available at any probed endpoint (all returned 404). Recommended fix: Start/configure the session test harness with the rotate_anthropic_after_rate_limit endpoint enabled (or update BASE_URL/path mapping to the actual mounted route) so at least one candidate path resolves non-404.
---

#### Test TC016
- **Test Name:** handle_sampling_failure branch matrix for compaction, rate-limit rotation, auth recovery, idle timeout, and empty response
- **Test Code:** [handle_sampling_failure_branch_matrix_for_compaction__rate_limit_rotation__auth_recovery__idle_timeout__and_empty_response.py](./handle_sampling_failure_branch_matrix_for_compaction__rate_limit_rotation__auth_recovery__idle_timeout__and_empty_response.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_9de02907-1048-465d-997b-c084ebd8ef1f.py", line 208, in <module>     test_bnd009_handle_sampling_failure_branch_matrix()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_9de02907-1048-465d-997b-c084ebd8ef1f.py", line 195, in test_bnd009_handle_sampling_failure_branch_matrix     assert any(            ~~~^         k in keys for k in (         ^^^^^^^^^^^^^^^^^^^^     ...<2 lines>...         )         ^     ), f"{case['name']}: missing recovery/result discriminator keys; keys={keys}"     ^ AssertionError: compact_on_error_true_boundary_context_window_min_nonzero: missing recovery/result discriminator keys; keys={'detail'}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test assumes successful/handled responses include recovery discriminator fields, but the endpoint returned an error payload with only `detail`, so the oracle is too strict for valid error shapes. Recommended fix: Gate the discriminator-key assertion behind a success condition (e.g., 2xx) and for non-2xx accept standard error JSON like `{detail: ...}` instead of requiring recovery/result keys.
---

#### Test TC017
- **Test Name:** process_conversation_turn handles optional agent/skill/startup/sampling/trace/schema/tooling/memory branches
- **Test Code:** [process_conversation_turn_handles_optional_agent_skill_startup_sampling_trace_schema_tooling_memory_branches.py](./process_conversation_turn_handles_optional_agent_skill_startup_sampling_trace_schema_tooling_memory_branches.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

#### Test TC018
- **Test Name:** ensure_fresh propagates credential resolution error and skips cache update
- **Test Code:** [ensure_fresh_propagates_credential_resolution_error_and_skips_cache_update.py](./ensure_fresh_propagates_credential_resolution_error_and_skips_cache_update.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_f6c9dff7-709f-4b73-a535-07dc12cff42b.py", line 57, in <module>     test_exc001_ensure_fresh_propagates_error_and_skips_cache_update()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_f6c9dff7-709f-4b73-a535-07dc12cff42b.py", line 17, in test_exc001_ensure_fresh_propagates_error_and_skips_cache_update     assert r.status_code == 200, f"Expected 200 preseed, got {r.status_code}: {r.text}"            ^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200 preseed, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test failed before exercising logic because the expected test helper endpoint `/test/live-auth/cache/preseed` is not available on the running service (HTTP 404). Recommended fix: Start the API in test mode or with the test routes enabled so `/test/live-auth/cache/preseed` (and related `/test/live-auth/*` endpoints) are registered.
---

#### Test TC019
- **Test Name:** record_rate_limit returns manager error and handles marked/cache branch combinations
- **Test Code:** [record_rate_limit_returns_manager_error_and_handles_marked_cache_branch_combinations.py](./record_rate_limit_returns_manager_error_and_handles_marked_cache_branch_combinations.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_a874abf3-23e6-4f1e-9eb7-6807d8c80133.py", line 102, in <module>     test_exc002_record_rate_limit_error_and_marked_cache_branches()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_a874abf3-23e6-4f1e-9eb7-6807d8c80133.py", line 23, in test_exc002_record_rate_limit_error_and_marked_cache_branches     assert any(k in body for k in ("error", "err", "message")), f"Expected error-shaped response, got keys: {list(body.keys())}"            ~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected error-shaped response, got keys: ['detail']
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test's error-shape oracle is too strict and misses the API's valid error field `detail`, causing a false failure despite correct error propagation. Recommended fix: Update the assertion to accept `detail` as an error key (e.g., check for any of `error`, `err`, `message`, or `detail`) or assert non-2xx plus non-empty JSON error payload.
---

#### Test TC020
- **Test Name:** store_path_exists false when env/path is missing
- **Test Code:** [store_path_exists_false_when_env_path_is_missing.py](./store_path_exists_false_when_env_path_is_missing.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_60d4b429-a7f0-4d42-9b43-7302ce283f92.py", line 58, in <module>     test_store_path_exists_false_when_env_or_path_missing()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_60d4b429-a7f0-4d42-9b43-7302ce283f92.py", line 32, in test_store_path_exists_false_when_env_or_path_missing     assert last_resp.status_code == 200, f"Expected 200, got {last_resp.status_code}: {last_resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test probes guessed HTTP routes for `store_path_exists`, but the service exposes none of those endpoints so every request returns 404. Recommended fix: Update the test to call the actual documented route (or API spec-discovered path) for `store_path_exists` instead of iterating guessed endpoint candidates.
---

#### Test TC021
- **Test Name:** resolve_headers propagates resolve_credential failure
- **Test Code:** [resolve_headers_propagates_resolve_credential_failure.py](./resolve_headers_propagates_resolve_credential_failure.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_df22856d-a5d1-46f9-a40d-e2270f3bcf44.py", line 98, in <module>     test_exc004_resolve_headers_propagates_resolve_credential_failure()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_df22856d-a5d1-46f9-a40d-e2270f3bcf44.py", line 45, in test_exc004_resolve_headers_propagates_resolve_credential_failure     assert failure_response is not None, "resolve_headers endpoint not found or method unsupported on all attempts"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: resolve_headers endpoint not found or method unsupported on all attempts
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test assumes a specific route/method (`POST /resolve_headers`) and treats only non-404/405 as valid, but the service under test does not expose that exact endpoint contract so all probes are rejected. Recommended fix: Update the test to call the actual implemented resolve-headers API path/method (or discover it from the service spec) instead of hardcoding `/resolve_headers` with POST.
---

#### Test TC022
- **Test Name:** record_selected_rate_limit handles no-selected-account and store write errors
- **Test Code:** [record_selected_rate_limit_handles_no_selected_account_and_store_write_errors.py](./record_selected_rate_limit_handles_no_selected_account_and_store_write_errors.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_9cfd0e4f-722f-42c8-ba5d-e72fd1e4fa1d.py", line 67, in <module>     test_exc005_record_selected_rate_limit_branches()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_9cfd0e4f-722f-42c8-ba5d-e72fd1e4fa1d.py", line 21, in test_exc005_record_selected_rate_limit_branches     assert r_missing.status_code == 200, f"Expected 200 for missing-account path, got {r_missing.status_code}: {r_missing.text}"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200 for missing-account path, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test calls a non-existent or incorrect endpoint (`POST /test/EXC005`), so it gets a routing 404 before exercising the missing-account logic. Recommended fix: Update the test to use the actual implemented test hook/path for `record_selected_rate_limit` (or add that route in the service) and assert against that endpoint’s real contract.
---

#### Test TC023
- **Test Name:** disabled_reason falls back for non-endpoint errors
- **Test Code:** [disabled_reason_falls_back_for_non_endpoint_errors.py](./disabled_reason_falls_back_for_non_endpoint_errors.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_e81bde11-d46f-4c2b-a0a6-edd9ae1f2b82.py", line 83, in <module>     test_exc006_disabled_reason_fallback_and_endpoint_code()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_e81bde11-d46f-4c2b-a0a6-edd9ae1f2b82.py", line 81, in test_exc006_disabled_reason_fallback_and_endpoint_code     raise AssertionError(f"Could not find working disabled_reason endpoint under base URL {BASE_URL}. Last error: {last_err}") AssertionError: Could not find working disabled_reason endpoint under base URL http://127.0.0.1:8080. Last error: None
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test never found any reachable/implemented disabled_reason route at 127.0.0.1:8080 (all candidate paths returned 404/405), so no assertions about logic were executed. Recommended fix: Start the correct service on port 8080 or point BASE_URL to the running API host/port that exposes the disabled_reason endpoint.
---

#### Test TC024
- **Test Name:** set_file_private no-op does not error on invalid path
- **Test Code:** [set_file_private_no_op_does_not_error_on_invalid_path.py](./set_file_private_no_op_does_not_error_on_invalid_path.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_4d258534-5cdd-4144-8dba-5e66e80b73fb.py", line 37, in <module>     test_set_file_private_noop_invalid_path()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_4d258534-5cdd-4144-8dba-5e66e80b73fb.py", line 27, in test_set_file_private_noop_invalid_path     assert any(k in data for k in expected_keys), (            ~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected one of keys {'success', 'ok', 'message', 'error', 'status'} in response JSON, got keys={'detail'}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test oracle is too strict about response JSON keys and failed when the API returned a valid non-crashing error object using only the key 'detail'. Recommended fix: Relax the assertion to accept 'detail' (or any non-empty JSON object) as a valid no-op/error indicator instead of requiring one of {'ok','success','status','message','error'}.
---

#### Test TC025
- **Test Name:** rotate_anthropic_after_rate_limit returns false on rate-limit recording/rotation failures
- **Test Code:** [rotate_anthropic_after_rate_limit_returns_false_on_rate_limit_recording_rotation_failures.py](./rotate_anthropic_after_rate_limit_returns_false_on_rate_limit_recording_rotation_failures.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_e0d725ec-7c7d-4def-acdd-a11a808dcfb3.py", line 101, in <module>     test_exc008_rotate_anthropic_after_rate_limit_failure_paths()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_e0d725ec-7c7d-4def-acdd-a11a808dcfb3.py", line 33, in test_exc008_rotate_anthropic_after_rate_limit_failure_paths     result, _ = _assert_bool_response(resp)                 ~~~~~~~~~~~~~~~~~~~~~^^^^^^   File "/tmp/ts_rs_e0d725ec-7c7d-4def-acdd-a11a808dcfb3.py", line 10, in _assert_bool_response     assert resp.status_code == expected_status, f"Expected {expected_status}, got {resp.status_code}, body={resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200, got 404, body={"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test failed before exercising logic because the target API route `/rotate_anthropic_after_rate_limit` is not available on the running server (404 Not Found). Recommended fix: Start the correct service/version that exposes `POST /rotate_anthropic_after_rate_limit` (or mount the missing router) before running the test.
---

#### Test TC026
- **Test Name:** handle_sampling_failure chooses non-recovery paths when eligibility checks fail
- **Test Code:** [handle_sampling_failure_chooses_non_recovery_paths_when_eligibility_checks_fail.py](./handle_sampling_failure_chooses_non_recovery_paths_when_eligibility_checks_fail.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_18652077-68ec-47e4-9a94-2bb3794c1c47.py", line 153, in <module>     test_exc009_handle_sampling_failure_negative_branches()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_18652077-68ec-47e4-9a94-2bb3794c1c47.py", line 122, in test_exc009_handle_sampling_failure_negative_branches     assert resp.status_code == 200, f"{case['name']}: expected 200, got {resp.status_code}, body={resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: rate_limited_rotate_false: expected 200, got 404, body={"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test targets /handle_sampling_failure on localhost:8080 but that route is not available in the running service, causing a 404 before any case logic executes. Recommended fix: Start the correct backend build (or test fixture server) that exposes POST /handle_sampling_failure on port 8080, or update BASE_URL/path to the actual deployed endpoint.
---

#### Test TC027
- **Test Name:** process_conversation_turn tolerates missing optional context and surfaces downstream errors
- **Test Code:** [process_conversation_turn_tolerates_missing_optional_context_and_surfaces_downstream_errors.py](./process_conversation_turn_tolerates_missing_optional_context_and_surfaces_downstream_errors.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

#### Test TC001
- **Test Name:** ensure_fresh returns resolved credential and updates cache when mutex lock succeeds
- **Test Code:** [ensure_fresh_returns_resolved_credential_and_updates_cache_when_mutex_lock_succeeds.py](./ensure_fresh_returns_resolved_credential_and_updates_cache_when_mutex_lock_succeeds.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_2ab63431-2b93-4ba5-8b34-5857fe37020e.py", line 60, in <module>     test_tc001_ensure_fresh_returns_resolved_credential_and_updates_cache()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_2ab63431-2b93-4ba5-8b34-5857fe37020e.py", line 24, in test_tc001_ensure_fresh_returns_resolved_credential_and_updates_cache     assert create_resp.status_code == 201, f"Expected 201 creating instance, got {create_resp.status_code}: {create_resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 201 creating instance, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test failed before exercising ensure_fresh because the expected test-only endpoint POST /test/live-auth-instances is not available on the running service (returned 404). Recommended fix: Start/configure the API server build that exposes the /test/live-auth-instances routes (or mount those test routes in the app) before running this test.
---

#### Test TC002
- **Test Name:** record_rate_limit marks selected account and clears cache when mark exists
- **Test Code:** [record_rate_limit_marks_selected_account_and_clears_cache_when_mark_exists.py](./record_rate_limit_marks_selected_account_and_clears_cache_when_mark_exists.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_15e8bf40-a300-493d-9aa3-e4266f405493.py", line 30, in <module>     test_tc002_record_rate_limit_marks_selected_account_and_clears_cache()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_15e8bf40-a300-493d-9aa3-e4266f405493.py", line 18, in test_tc002_record_rate_limit_marks_selected_account_and_clears_cache     assert resp.status_code == 200, f"Expected 200, got {resp.status_code}, body={resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200, got 404, body={"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test endpoint URL `/test/record_rate_limit` is not available on the running service (server route missing or wrong base URL/port), causing an HTTP 404 before business logic executes. Recommended fix: Start the correct test harness/service exposing `POST /test/record_rate_limit` on `127.0.0.1:8080` (or update `base_url`/path to the actual mounted route).
---

#### Test TC003
- **Test Name:** store_path_exists reflects filesystem presence of account store path
- **Test Code:** [store_path_exists_reflects_filesystem_presence_of_account_store_path.py](./store_path_exists_reflects_filesystem_presence_of_account_store_path.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_4d4f0582-a393-4373-ba48-823d1706c02b.py", line 68, in <module>     test_tc003_store_path_exists_reflects_filesystem_presence()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_4d4f0582-a393-4373-ba48-823d1706c02b.py", line 45, in test_tc003_store_path_exists_reflects_filesystem_presence     assert response is not None, "Could not find a reachable store_path_exists endpoint"            ^^^^^^^^^^^^^^^^^^^^ AssertionError: Could not find a reachable store_path_exists endpoint
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test failed because it guessed multiple endpoint paths and none matched the actual API route, so no non-404 response was found. Recommended fix: Use the service’s documented/openapi route for `store_path_exists` (single known endpoint) instead of probing candidate paths.
---

#### Test TC004
- **Test Name:** resolve_headers builds header mutation from resolved credential
- **Test Code:** [resolve_headers_builds_header_mutation_from_resolved_credential.py](./resolve_headers_builds_header_mutation_from_resolved_credential.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_f39f8b03-afbe-4f1b-9602-2499a1a4c3d5.py", line 84, in <module>     test_tc004_resolve_headers_builds_header_mutation_from_resolved_credential()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_f39f8b03-afbe-4f1b-9602-2499a1a4c3d5.py", line 27, in test_tc004_resolve_headers_builds_header_mutation_from_resolved_credential     assert setup_resp.status_code == 200, (            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Failed to configure manager spy: 404 {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test depends on a test-only spy configuration endpoint (/test-doubles/manager/spy) that is not available in the running server, causing an immediate 404 before exercising resolve_headers. Recommended fix: Run the service in a test/debug mode that registers /test-doubles/manager/spy (and related spy endpoints), or deploy a build that includes these test-double routes.
---

#### Test TC005
- **Test Name:** record_selected_rate_limit updates selected account fields and returns account name
- **Test Code:** [record_selected_rate_limit_updates_selected_account_fields_and_returns_account_name.py](./record_selected_rate_limit_updates_selected_account_fields_and_returns_account_name.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_9082f0a4-2309-421c-95a7-b5532323bd45.py", line 151, in <module>     test_tc005_record_selected_rate_limit_updates_selected_account()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_9082f0a4-2309-421c-95a7-b5532323bd45.py", line 53, in test_tc005_record_selected_rate_limit_updates_selected_account     assert created, "Could not initialize store with selectable account acct_a"            ^^^^^^^ AssertionError: Could not initialize store with selectable account acct_a
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test could not create seed data because none of its guessed initialization endpoints/payload shapes matched the service API, so setup failed before exercising the target logic. Recommended fix: Update the test to use the actual documented account-creation/init endpoint and request schema for this service (instead of probing generic paths), then assert creation success from that known contract.
---

#### Test TC006
- **Test Name:** disabled_reason returns oauth error code when endpoint error includes oauth_error
- **Test Code:** [disabled_reason_returns_oauth_error_code_when_endpoint_error_includes_oauth_error.py](./disabled_reason_returns_oauth_error_code_when_endpoint_error_includes_oauth_error.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_0cb62156-9924-4367-9343-df6e4b4d747c.py", line 24, in <module>     test_tc006_disabled_reason_oauth_error_mapping()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_0cb62156-9924-4367-9343-df6e4b4d747c.py", line 15, in test_tc006_disabled_reason_oauth_error_mapping     assert response.status_code == 200, f"Expected 200, got {response.status_code}: {response.text}"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test targets POST /disabled_reason on localhost:8080, but that route is not available in the running service, resulting in a 404 before application logic is exercised. Recommended fix: Start the correct API service/version that exposes POST /disabled_reason (or run the test against the proper base URL/port where that endpoint is registered).
---

#### Test TC007
- **Test Name:** set_file_private is a no-op and does not error on valid path
- **Test Code:** [set_file_private_is_a_no_op_and_does_not_error_on_valid_path.py](./set_file_private_is_a_no_op_and_does_not_error_on_valid_path.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_49be53bb-357f-4615-af2b-3d1c31e55a14.py", line 48, in <module>     test_tc007_set_file_private_noop_valid_path()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_49be53bb-357f-4615-af2b-3d1c31e55a14.py", line 27, in test_tc007_set_file_private_noop_valid_path     assert response is not None, "Could not find set_file_private endpoint (all candidate routes returned 404)"            ^^^^^^^^^^^^^^^^^^^^ AssertionError: Could not find set_file_private endpoint (all candidate routes returned 404)
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The service does not expose any of the expected set_file_private routes, so every candidate request returns 404 and the test cannot reach the feature. Recommended fix: Implement and register a POST set_file_private endpoint (or one documented equivalent route) that accepts a JSON body with `path` and returns 200/204 for valid paths.
---

#### Test TC008
- **Test Name:** rotate_anthropic_after_rate_limit rotates successfully for anthropic rate-limit case
- **Test Code:** [rotate_anthropic_after_rate_limit_rotates_successfully_for_anthropic_rate_limit_case.py](./rotate_anthropic_after_rate_limit_rotates_successfully_for_anthropic_rate_limit_case.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_82c9ecfc-1300-482c-ba94-1927e85da607.py", line 56, in <module>     test_tc008_rotate_anthropic_after_rate_limit_success()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_82c9ecfc-1300-482c-ba94-1927e85da607.py", line 25, in test_tc008_rotate_anthropic_after_rate_limit_success     assert setup_resp.status_code == 200, f"Expected 200 from setup, got {setup_resp.status_code}: {setup_resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200 from setup, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test failed before exercising logic because the expected setup endpoint `/test/session/setup` is not available on the running service (HTTP 404). Recommended fix: Start the correct test harness/service build that exposes `/test/session/setup` on port 8080 (or point BASE_URL to that service) before running TC008.
---

#### Test TC009
- **Test Name:** handle_sampling_failure recovers via anthropic rate-limit rotation path
- **Test Code:** [handle_sampling_failure_recovers_via_anthropic_rate_limit_rotation_path.py](./handle_sampling_failure_recovers_via_anthropic_rate_limit_rotation_path.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_7bee156f-a315-48d3-9038-6dfbd4f950f2.py", line 47, in <module>     test_tc009_handle_sampling_failure_rate_limit_rotation_recovery()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_7bee156f-a315-48d3-9038-6dfbd4f950f2.py", line 21, in test_tc009_handle_sampling_failure_rate_limit_rotation_recovery     assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test posts to /handle_sampling_failure on localhost:8080, but that route is not available in the running service, yielding a 404 before business logic executes. Recommended fix: Start the correct API service/version that exposes POST /handle_sampling_failure (or configure BASE_URL to the host/path where this endpoint is actually mounted, e.g., including any /api prefix).
---

#### Test TC010
- **Test Name:** process_conversation_turn succeeds with populated optional context and structured output
- **Test Code:** [process_conversation_turn_succeeds_with_populated_optional_context_and_structured_output.py](./process_conversation_turn_succeeds_with_populated_optional_context_and_structured_output.py)
- **Test Error:** Traceback (most recent call last):   File "/tmp/ts_rs_7162d0dc-f002-49ac-97b4-016e17eef096.py", line 119, in <module>     test_tc010_process_conversation_turn_happy_path()     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^   File "/tmp/ts_rs_7162d0dc-f002-49ac-97b4-016e17eef096.py", line 62, in test_tc010_process_conversation_turn_happy_path     assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text}"            ^^^^^^^^^^^^^^^^^^^^^^^ AssertionError: Expected 200, got 404: {"detail":"404: Not Found"}
- **Status:** ❌ Failed
- **Severity:** HIGH
- **Analysis / Findings:** The test targets http://127.0.0.1:8080/process_conversation_turn but the running service does not expose that route (wrong base URL/port or endpoint path), resulting in HTTP 404 before application logic runs. Recommended fix: Start the correct API service and point the test to the actual conversation-turn endpoint (update base_url/path from /process_conversation_turn to the implemented route, e.g., via a configurable API_BASE_URL).
---

#### Test TC038
- **Test Name:** Brain F005: self-improvement run pipeline (gating, categorization, sources, links, stamp)
- **Test Code:** [Brain_F005__self_improvement_run_pipeline__gating__categorization__sources__links__stamp.py](./Brain_F005__self_improvement_run_pipeline__gating__categorization__sources__links__stamp.py)
- **Status:** ✅ Passed
- **Analysis / Findings:** Test passed. All assertions succeeded and the expected behavior was verified.
---

## 3️⃣ Coverage & Matching Metrics

- **26.32%** of tests passed

| Requirement | Total Tests | ✅ Passed | ❌ Failed |
|---|---|---|---|
| R001: Ungrouped | 38 | 10 | 28 |
| **Total** | 38 | 10 | 28 |

---

## 4️⃣ Key Gaps / Risks
- [routing_404] ensure_fresh returns resolved credential and updates cache when mutex lock succeeds
- [routing_404] record_rate_limit marks selected account and clears cache when mark exists
- [assertion] store_path_exists reflects filesystem presence of account store path
- [routing_404] resolve_headers builds header mutation from resolved credential
- [assertion] record_selected_rate_limit updates selected account fields and returns account name
- [routing_404] disabled_reason returns oauth error code when endpoint error includes oauth_error
- [routing_404] set_file_private is a no-op and does not error on valid path
- [routing_404] rotate_anthropic_after_rate_limit rotates successfully for anthropic rate-limit case
- [routing_404] handle_sampling_failure recovers via anthropic rate-limit rotation path
- [routing_404] process_conversation_turn succeeds with populated optional context and structured output
- [routing_404] ensure_fresh caches credential when mutex lock succeeds and still returns credential when lock is poisoned
- [routing_404] record_rate_limit clears cache only when manager marks an account and cache lock succeeds
- [routing_404] store_path_exists reflects filesystem state from env-configured store path
- [routing_404] resolve_headers maps resolved credential into header mutation
- [routing_404] record_selected_rate_limit marks selected account when found and handles missing account/selection
- [routing_404] disabled_reason returns oauth_error code when present, otherwise permanent_oauth_error
- [routing_404] set_file_private is a no-op and does not fail for diverse paths
- [routing_404] rotate_anthropic_after_rate_limit returns true only on anthropic + successful mark + usable accounts + fresh credential
- [assertion] handle_sampling_failure branch matrix for compaction, rate-limit rotation, auth recovery, idle timeout, and empty response
- [routing_404] ensure_fresh propagates credential resolution error and skips cache update
- [assertion] record_rate_limit returns manager error and handles marked/cache branch combinations
- [routing_404] store_path_exists false when env/path is missing
- [assertion] resolve_headers propagates resolve_credential failure
- [routing_404] record_selected_rate_limit handles no-selected-account and store write errors
- [assertion] disabled_reason falls back for non-endpoint errors
- [assertion] set_file_private no-op does not error on invalid path
- [routing_404] rotate_anthropic_after_rate_limit returns false on rate-limit recording/rotation failures
- [routing_404] handle_sampling_failure chooses non-recovery paths when eligibility checks fail
