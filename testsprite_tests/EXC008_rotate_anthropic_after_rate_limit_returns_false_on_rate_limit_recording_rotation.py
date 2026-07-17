import requests

BASE_URL = "http://127.0.0.1:8080"

def _post_json(path, payload):
    resp = requests.post(f"{BASE_URL}{path}", json=payload, timeout=10)
    return resp

def _assert_bool_response(resp, expected_status=200):
    assert resp.status_code == expected_status, f"Expected {expected_status}, got {resp.status_code}, body={resp.text}"
    data = resp.json()
    assert isinstance(data, dict), f"Expected JSON object, got: {type(data)}"
    assert "result" in data, f"Missing 'result' key in response: {data}"
    assert isinstance(data["result"], bool), f"'result' must be bool, got: {type(data['result'])}"
    return data["result"], data

def test_exc008_rotate_anthropic_after_rate_limit_failure_paths():
    endpoint = "/rotate_anthropic_after_rate_limit"

    # Subcase A: missing sampling config => false
    payload_missing_config = {
        "adapter": "anthropic",
        "session": {
            "id": "sess-missing-config"
        },
        "live": {
            "record_rate_limit_outcome": "ok_some",
            "has_usable_accounts": True,
            "ensure_fresh_outcome": "ok"
        }
    }
    resp = _post_json(endpoint, payload_missing_config)
    result, _ = _assert_bool_response(resp)
    assert result is False, "Expected false when sampling config is missing"

    # Subcase B: non-anthropic adapter => false
    payload_non_anthropic = {
        "adapter": "openai",
        "session": {
            "id": "sess-non-anthropic",
            "sampling_config": {"enabled": True}
        },
        "live": {
            "record_rate_limit_outcome": "ok_some",
            "has_usable_accounts": True,
            "ensure_fresh_outcome": "ok"
        }
    }
    resp = _post_json(endpoint, payload_non_anthropic)
    result, _ = _assert_bool_response(resp)
    assert result is False, "Expected false for non-anthropic adapter"

    # Base valid ACP + anthropic setup for failure branches
    base = {
        "adapter": "anthropic",
        "session": {
            "id": "sess-anthropic",
            "sampling_config": {"enabled": True, "rate_limit_sampling": 1.0}
        },
        "live": {
            "has_usable_accounts": True,
            "ensure_fresh_outcome": "ok"
        }
    }

    # (1) record_rate_limit(...) -> Err => false
    p1 = dict(base)
    p1["live"] = dict(base["live"])
    p1["live"]["record_rate_limit_outcome"] = "err"
    resp = _post_json(endpoint, p1)
    result, _ = _assert_bool_response(resp)
    assert result is False, "Expected false when record_rate_limit returns Err"

    # (2) record_rate_limit(...) -> Ok(None) => false
    p2 = dict(base)
    p2["live"] = dict(base["live"])
    p2["live"]["record_rate_limit_outcome"] = "ok_none"
    resp = _post_json(endpoint, p2)
    result, _ = _assert_bool_response(resp)
    assert result is False, "Expected false when record_rate_limit returns Ok(None)"

    # (3) record_rate_limit(...) -> Ok(Some), but has_usable_accounts() false => false
    p3 = dict(base)
    p3["live"] = dict(base["live"])
    p3["live"]["record_rate_limit_outcome"] = "ok_some"
    p3["live"]["has_usable_accounts"] = False
    resp = _post_json(endpoint, p3)
    result, _ = _assert_bool_response(resp)
    assert result is False, "Expected false when no usable accounts are available"

    # (4) has usable accounts but ensure_fresh().await Err => false
    p4 = dict(base)
    p4["live"] = dict(base["live"])
    p4["live"]["record_rate_limit_outcome"] = "ok_some"
    p4["live"]["has_usable_accounts"] = True
    p4["live"]["ensure_fresh_outcome"] = "err"
    resp = _post_json(endpoint, p4)
    result, _ = _assert_bool_response(resp)
    assert result is False, "Expected false when ensure_fresh fails"

test_exc008_rotate_anthropic_after_rate_limit_failure_paths()