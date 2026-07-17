import requests

def test_tc002_record_rate_limit_marks_selected_account_and_clears_cache():
    base_url = "http://127.0.0.1:8080"

    payload = {
        "mock": {
            "record_selected_rate_limit_return": {"ok": "acct_a"},
            "cache_prefill": {"credential": "valid_credential_value"}
        },
        "input": {
            "retry_after": 30,
            "reason": "rate limited"
        }
    }

    resp = requests.post(f"{base_url}/test/record_rate_limit", json=payload, timeout=10)
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}, body={resp.text}"

    data = resp.json()
    assert isinstance(data, dict), f"Expected JSON object, got: {type(data)}"

    assert "ok" in data, f"Expected 'ok' field in response, got: {data}"
    assert data["ok"] is not None, f"Expected ok to be Some(...), got: {data}"
    assert data["ok"] == "acct_a", f"Expected ok='acct_a', got: {data['ok']}"

    assert "cache" in data, f"Expected 'cache' field in response for post-call state, got: {data}"
    assert data["cache"] is None, f"Expected cache to be cleared (None), got: {data['cache']}"

test_tc002_record_rate_limit_marks_selected_account_and_clears_cache()