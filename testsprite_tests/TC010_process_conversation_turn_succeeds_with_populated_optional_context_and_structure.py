import requests
import uuid
import time


def test_tc010_process_conversation_turn_happy_path():
    base_url = "http://127.0.0.1:8080"

    session_id = f"tc010-session-{uuid.uuid4()}"
    parent_session_id = f"tc010-parent-{uuid.uuid4()}"

    payload = {
        "session_id": session_id,
        "turn_id": f"turn-{uuid.uuid4()}",
        "input": {
            "role": "user",
            "content": "Please produce a short structured status update."
        },
        "active_agent_type": "planner_agent",
        "active_skill": "structured_status_skill",
        "startup_hints": {
            "mode": "subagent",
            "parent_session_id": parent_session_id
        },
        "sampling_config": {
            "temperature": 0.2,
            "top_p": 0.9,
            "reasoning_effort": "medium"
        },
        "trace_gcs_config": {
            "bucket": "test-traces-bucket",
            "prefix": f"tc010/{session_id}"
        },
        "json_schema": {
            "name": "status_update",
            "schema": {
                "type": "object",
                "additionalProperties": False,
                "properties": {
                    "summary": {"type": "string"},
                    "priority": {"type": "string", "enum": ["low", "medium", "high"]},
                    "next_steps": {
                        "type": "array",
                        "items": {"type": "string"}
                    }
                },
                "required": ["summary", "priority", "next_steps"]
            }
        },
        "features": {
            "structured_output_tool": True,
            "memory_reminder": True
        }
    }

    resp = requests.post(
        f"{base_url}/process_conversation_turn",
        json=payload,
        timeout=30
    )

    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text}"

    data = resp.json()
    assert isinstance(data, dict), f"Expected JSON object, got: {type(data)}"
    assert "ok" in data, f"Missing 'ok' field in response: {data}"
    assert data["ok"] is True, f"Expected ok=True, got: {data}"

    assert "turn_outcome" in data, f"Missing 'turn_outcome' in response: {data}"
    turn_outcome = data["turn_outcome"]
    assert isinstance(turn_outcome, dict), f"turn_outcome should be object, got: {type(turn_outcome)}"

    # Verify representative observable side effects in current code path:
    # 1) turn start recorded (observable via a started_at/turn metadata field)
    observed_turn_start = any(
        key in turn_outcome for key in ["started_at", "turn_started_at", "turn_start_time", "turn_id"]
    ) or any(
        key in data for key in ["started_at", "turn_started_at", "turn_start_time"]
    )
    assert observed_turn_start, f"No observable turn-start marker found in response: {data}"

    # 2) sampling config read (observable via echo/usage/meta mentioning reasoning effort or sampling)
    sampling_observed = False
    candidate_objects = [data, turn_outcome]
    for obj in candidate_objects:
        if not isinstance(obj, dict):
            continue
        for key in ["sampling_config", "effective_sampling_config", "debug", "meta", "usage"]:
            if key in obj:
                val = obj[key]
                text = str(val).lower()
                if "reasoning_effort" in text or "sampling" in text or "temperature" in text:
                    sampling_observed = True
                    break
        if sampling_observed:
            break
    assert sampling_observed, f"No observable evidence sampling config was read in response: {data}"

    # 3) no error
    assert "error" not in data or data["error"] in (None, "", {}), f"Unexpected error in response: {data}"

    # Optional follow-up check endpoint if present for stronger observability
    # (kept non-fatal if endpoint doesn't exist)
    try:
        time.sleep(0.2)
        events_resp = requests.get(
            f"{base_url}/sessions/{session_id}/events",
            timeout=10
        )
        if events_resp.status_code == 200:
            events = events_resp.json()
            events_text = str(events).lower()
            assert "turn_start" in events_text or "turn_started" in events_text, \
                f"Events endpoint did not show turn start: {events}"
    except requests.RequestException:
        pass


test_tc010_process_conversation_turn_happy_path()