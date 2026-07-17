import requests

BASE_URL = "http://127.0.0.1:8080"

def test_tc009_handle_sampling_failure_rate_limit_rotation_recovery():
    payload = {
        "error": {
            "kind": "RateLimited"
        },
        "session": {
            "should_compact_on_error": False,
            "rotate_anthropic_after_rate_limit": True
        },
        "mocks": {
            "should_compact_on_error": False,
            "rotate_anthropic_after_rate_limit": True
        }
    }

    resp = requests.post(f"{BASE_URL}/handle_sampling_failure", json=payload, timeout=10)
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}: {resp.text}"

    data = resp.json()
    assert isinstance(data, dict), f"Expected JSON object, got: {type(data)}"

    ok = data.get("ok")
    assert ok is True, f"Expected ok=True, got: {ok}, body={data}"

    outcome = data.get("outcome")
    assert isinstance(outcome, dict), f"Expected outcome object, got: {outcome}"

    hard_failure = outcome.get("hard_failure")
    assert hard_failure in (False, None), f"Expected no hard failure, got: {hard_failure}"

    retry = outcome.get("retry")
    continue_ = outcome.get("continue")
    path = outcome.get("path")
    recovery = outcome.get("recovery")

    assert (
        retry is True
        or continue_ is True
        or path in ("retry", "continue", "recover", "rate_limit_rotation")
        or recovery in ("retry", "continue", "recover", "rate_limit_rotation")
    ), f"Expected retry/continue recovery semantics, got outcome={outcome}"

test_tc009_handle_sampling_failure_rate_limit_rotation_recovery()