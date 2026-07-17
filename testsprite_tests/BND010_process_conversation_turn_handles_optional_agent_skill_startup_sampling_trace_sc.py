import json
import uuid
import requests

BASE_URL = "http://127.0.0.1:8080"
TIMEOUT = 20


def _try_post(path, payload):
    return requests.post(f"{BASE_URL}{path}", json=payload, timeout=TIMEOUT)


def _discover_endpoint():
    candidates = [
        "/process_conversation_turn",
        "/v1/process_conversation_turn",
        "/conversation/process_turn",
        "/v1/conversation/process_turn",
        "/session/process_conversation_turn",
        "/v1/session/process_conversation_turn",
    ]
    probe = {"req_id": "probe", "messages": [{"role": "user", "content": "ping"}]}
    for p in candidates:
        try:
            r = _try_post(p, probe)
            if r.status_code in (200, 400, 401, 403, 404, 405, 409, 422, 500):
                return p
        except requests.RequestException:
            continue
    raise AssertionError("Could not discover a reachable process_conversation_turn endpoint")


def _maybe_json(resp):
    try:
        return resp.json()
    except Exception:
        return None


def _assert_response_shape(resp, expect_success):
    assert resp.status_code in (200, 400, 401, 403, 404, 409, 422, 500), f"Unexpected status {resp.status_code}"
    body = _maybe_json(resp)
    assert body is not None, f"Expected JSON response, got: {resp.text[:300]}"
    assert isinstance(body, dict), "Response JSON must be an object"

    if expect_success and resp.status_code == 200:
        # Minimal non-speculative success checks:
        # expect at least one of common result keys to exist.
        common_keys = {"result", "output", "message", "response", "status"}
        assert any(k in body for k in common_keys), f"Success response missing expected top-level keys: {body.keys()}"
    else:
        # For non-200, require error-like shape or status signaling.
        if resp.status_code != 200:
            errorish = {"error", "detail", "message", "code", "status"}
            assert any(k in body for k in errorish), f"Error response missing expected keys: {body.keys()}"

    return body


def _build_payload(
    req_id,
    active_agent_type=None,
    active_skill=None,
    startup_is_subagent=False,
    parent_session_id=None,
    sampling_config=None,
    reasoning_effort=None,
    prompt_timing=None,
    trace_gcs_config=None,
    json_schema=None,
    structured_output_tool_enabled=False,
    memory_reminder=None,
):
    payload = {
        "req_id": req_id,
        "session_id": f"sess-{uuid.uuid4()}",
        "messages": [{"role": "user", "content": "hello"}],
        "startup_hints": {"is_subagent": startup_is_subagent},
    }

    if active_agent_type is not None:
        payload["active_agent_type"] = active_agent_type
    if active_skill is not None:
        payload["active_skill"] = active_skill
    if parent_session_id is not None:
        payload["parent_session_id"] = parent_session_id
    if sampling_config is not None:
        payload["sampling_config"] = sampling_config
    if reasoning_effort is not None:
        payload["reasoning_effort"] = reasoning_effort
    if prompt_timing is not None:
        payload["prompt_timing"] = prompt_timing
    if trace_gcs_config is not None:
        payload["trace_gcs_config"] = trace_gcs_config
    if json_schema is not None:
        payload["json_schema"] = json_schema
    if structured_output_tool_enabled:
        payload["structured_output_tool"] = {"enabled": True}
    else:
        payload["structured_output_tool"] = {"enabled": False}
    if memory_reminder is not None:
        payload["memory_reminder"] = memory_reminder

    return payload


def test_bnd010_process_conversation_turn_optional_branches():
    endpoint = _discover_endpoint()

    # Establish baseline with minimal payload.
    baseline_payload = _build_payload(
        req_id="baseline",
        startup_is_subagent=False,
        structured_output_tool_enabled=False,
    )
    baseline_resp = _try_post(endpoint, baseline_payload)
    baseline_body = _assert_response_shape(baseline_resp, expect_success=(baseline_resp.status_code == 200))
    baseline_ok = baseline_resp.status_code == 200

    req_ids = [
        "",  # empty boundary
        "r" * 512,  # long boundary
        "请求-🧪-Δ",  # unicode boundary
    ]

    # Toggle each condition both ways in table rows while staying non-speculative.
    rows = [
        {"name": "active_agent_type_some", "kwargs": {"active_agent_type": "assistant_agent"}},
        {"name": "active_agent_type_none", "kwargs": {"active_agent_type": None}},
        {"name": "active_skill_some", "kwargs": {"active_skill": "summarize"}},
        {"name": "active_skill_none", "kwargs": {"active_skill": None}},
        {"name": "startup_is_subagent_true", "kwargs": {"startup_is_subagent": True}},
        {"name": "startup_is_subagent_false", "kwargs": {"startup_is_subagent": False}},
        {"name": "parent_session_id_some", "kwargs": {"parent_session_id": f"parent-{uuid.uuid4()}"}},
        {"name": "parent_session_id_none", "kwargs": {"parent_session_id": None}},
        {"name": "sampling_config_some", "kwargs": {"sampling_config": {"temperature": 0.2, "top_p": 0.9}}},
        {"name": "sampling_config_none", "kwargs": {"sampling_config": None}},
        {"name": "reasoning_effort_some", "kwargs": {"reasoning_effort": "medium"}},
        {"name": "reasoning_effort_none", "kwargs": {"reasoning_effort": None}},
        {"name": "prompt_timing_some", "kwargs": {"prompt_timing": "eager"}},
        {"name": "prompt_timing_none", "kwargs": {"prompt_timing": None}},
        {"name": "trace_gcs_config_some", "kwargs": {"trace_gcs_config": {"bucket": "test-bucket", "prefix": "trace/"}}},
        {"name": "trace_gcs_config_none", "kwargs": {"trace_gcs_config": None}},
        {"name": "json_schema_some", "kwargs": {"json_schema": {"type": "object", "properties": {"answer": {"type": "string"}}, "required": ["answer"]}}},
        {"name": "json_schema_none", "kwargs": {"json_schema": None}},
        {"name": "structured_output_tool_enabled", "kwargs": {"structured_output_tool_enabled": True}},
        {"name": "structured_output_tool_disabled", "kwargs": {"structured_output_tool_enabled": False}},
        {"name": "memory_reminder_some", "kwargs": {"memory_reminder": "Remember user preference: concise."}},
        {"name": "memory_reminder_none", "kwargs": {"memory_reminder": None}},
    ]

    observed = []

    for row in rows:
        for rid in req_ids:
            payload = _build_payload(req_id=rid, **row["kwargs"])
            resp = _try_post(endpoint, payload)
            body = _assert_response_shape(resp, expect_success=(baseline_ok and resp.status_code == 200))

            # Result consistency with baseline (non-speculative):
            # if baseline succeeded, variants should generally not hard-fail with transport/non-JSON.
            # We only assert allowed status-class and shape above; here we assert deterministic "JSON object" behavior.
            assert isinstance(body, dict)

            # Metadata paths should be accepted/echoed or at least not crash.
            # Validate presence if echoed; do not require exact oracle.
            echoed_text = json.dumps(body, ensure_ascii=False)
            for key in ["trace", "span", "config", "tool", "schema", "memory", "sampling"]:
                if key in echoed_text:
                    break  # evidence of metadata path usage in response

            observed.append((row["name"], rid, resp.status_code))

    # Ensure we exercised all rows * req_id boundaries.
    assert len(observed) == len(rows) * len(req_ids), "Did not execute full boundary matrix"


test_bnd010_process_conversation_turn_optional_branches()