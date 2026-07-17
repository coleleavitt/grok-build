import requests
import uuid

BASE_URL = "http://127.0.0.1:8080"


def _assert_error_response(resp, context: str):
    assert resp.status_code >= 400, f"{context}: expected error status, got {resp.status_code}, body={resp.text}"
    try:
        data = resp.json()
    except Exception as e:
        raise AssertionError(f"{context}: response is not valid JSON: {e}; body={resp.text}")

    assert isinstance(data, dict), f"{context}: expected JSON object, got {type(data)}"
    # Flexible error-shape assertions to tolerate implementation differences while
    # still verifying proper error propagation.
    has_error_key = any(k in data for k in ("error", "err", "message", "detail", "code"))
    assert has_error_key, f"{context}: expected an error-shaped object, got keys={list(data.keys())}"


def test_process_conversation_turn_error_propagation_optional_paths():
    # False-side coverage payload: many optional fields omitted/None
    false_side_payload = {
        "method": "process_conversation_turn",
        "params": {
            "session_id": f"sess-{uuid.uuid4()}",
            "turn": {
                "role": "user",
                "content": "Trigger downstream failure via impossible tool/sampler call."
            },
            "json_schema": None,
            "trace_gcs_config": None,
            "active_agent_lock": None,
            "active_skill_lock": None,
            "subagent_startup_hints": {
                "is_subagent": False,
                "hints": []
            },
            # sampling_config intentionally absent
            "force_error": True,
            "induce_failure": "sampler_or_tool"
        },
        "id": str(uuid.uuid4()),
        "jsonrpc": "2.0"
    }

    r1 = requests.post(f"{BASE_URL}/", json=false_side_payload, timeout=30)
    _assert_error_response(r1, "false-side optional coverage")

    # True-side complementary payload: optional fields populated
    true_side_payload = {
        "method": "process_conversation_turn",
        "params": {
            "session_id": f"sess-{uuid.uuid4()}",
            "parent_session_id": f"parent-{uuid.uuid4()}",
            "turn": {
                "role": "user",
                "content": "Also trigger downstream failure even with full optional context."
            },
            "active_agent_lock": "agent_alpha",
            "active_skill_lock": "skill_beta",
            "subagent_startup_hints": {
                "is_subagent": True,
                "hints": ["startup:delegate", "priority:high"]
            },
            "sampling_config": {
                "temperature": 0.7,
                "top_p": 0.9,
                "reasoning_effort": "high"
            },
            "prompt_timing": {
                "enqueue_ms": 5,
                "build_ms": 12
            },
            "trace_gcs_config": {
                "bucket": "test-bucket",
                "prefix": "trace-prefix"
            },
            "structured_output_tool": {
                "enabled": True
            },
            "memory_reminder": "Remember to fail safely and return structured errors.",
            "json_schema": {
                "type": "object",
                "properties": {
                    "answer": {"type": "string"}
                },
                "required": ["answer"]
            },
            "force_error": True,
            "induce_failure": "sampler_or_tool"
        },
        "id": str(uuid.uuid4()),
        "jsonrpc": "2.0"
    }

    r2 = requests.post(f"{BASE_URL}/", json=true_side_payload, timeout=30)
    _assert_error_response(r2, "true-side optional coverage")


test_process_conversation_turn_error_propagation_optional_paths()