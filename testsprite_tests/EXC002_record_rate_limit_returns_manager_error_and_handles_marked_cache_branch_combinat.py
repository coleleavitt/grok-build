import requests

BASE_URL = "http://127.0.0.1:8080"

def test_exc002_record_rate_limit_error_and_marked_cache_branches():
    # Subcase 1: manager error propagation for invalid/unwritable store
    # Expected: Err is propagated by API as non-2xx with error-shaped response.
    r = requests.post(
        f"{BASE_URL}/record_rate_limit",
        json={
            "marked": 1,
            "message": "msg",
            "mock": {
                "manager_record_selected_rate_limit": {"result": "err", "error": "invalid_or_unwritable_store"},
                "lock_available": True
            }
        },
        timeout=10,
    )
    assert r.status_code >= 400, f"Expected error status for manager Err, got {r.status_code}: {r.text}"
    body = r.json()
    assert isinstance(body, dict), f"Expected JSON object, got: {type(body)}"
    assert any(k in body for k in ("error", "err", "message")), f"Expected error-shaped response, got keys: {list(body.keys())}"

    # Subcase (a): Ok(Some(name)) with lock available => cache set to None
    r = requests.post(
        f"{BASE_URL}/record_rate_limit",
        json={
            "marked": 1,
            "message": "msg",
            "mock": {
                "manager_record_selected_rate_limit": {"result": "ok", "value": "rate_limit_name"},
                "lock_available": True
            }
        },
        timeout=10,
    )
    assert 200 <= r.status_code < 300, f"Expected success, got {r.status_code}: {r.text}"
    body = r.json()
    assert isinstance(body, dict), f"Expected JSON object, got: {type(body)}"
    returned = body.get("result", body.get("value", body.get("data")))
    if isinstance(returned, dict) and "value" in returned:
        returned = returned["value"]
    assert returned == "rate_limit_name" or body.get("name") == "rate_limit_name", f"Expected Ok(Some(name)), got: {body}"
    # Branch effect: cache cleared to None
    cache_state = body.get("cache_selected_rate_limit") or body.get("cache")
    if isinstance(cache_state, dict):
        cache_val = cache_state.get("selected_rate_limit", cache_state.get("value"))
    else:
        cache_val = cache_state
    assert cache_val is None, f"Expected cache set to None when lock available and marked is Some, got: {body}"

    # Subcase (b): Ok(None) => cache unchanged
    r = requests.post(
        f"{BASE_URL}/record_rate_limit",
        json={
            "marked": 1,
            "message": "msg",
            "mock": {
                "manager_record_selected_rate_limit": {"result": "ok", "value": None},
                "lock_available": True,
                "initial_cache": "keep_me"
            }
        },
        timeout=10,
    )
    assert 200 <= r.status_code < 300, f"Expected success, got {r.status_code}: {r.text}"
    body = r.json()
    assert isinstance(body, dict), f"Expected JSON object, got: {type(body)}"
    returned = body.get("result", body.get("value", body.get("data")))
    if isinstance(returned, dict) and "value" in returned:
        returned = returned["value"]
    assert returned is None, f"Expected Ok(None), got: {body}"
    cache_state = body.get("cache_selected_rate_limit") or body.get("cache")
    if isinstance(cache_state, dict):
        cache_val = cache_state.get("selected_rate_limit", cache_state.get("value"))
    else:
        cache_val = cache_state
    assert cache_val == "keep_me", f"Expected cache unchanged for Ok(None), got: {body}"

    # Subcase (c): Ok(Some(name)) with poisoned/failed lock => still Ok(Some(name)) and no panic
    r = requests.post(
        f"{BASE_URL}/record_rate_limit",
        json={
            "marked": 1,
            "message": "msg",
            "mock": {
                "manager_record_selected_rate_limit": {"result": "ok", "value": "rate_limit_name_2"},
                "lock_available": False
            }
        },
        timeout=10,
    )
    assert 200 <= r.status_code < 300, f"Expected success despite lock failure, got {r.status_code}: {r.text}"
    body = r.json()
    assert isinstance(body, dict), f"Expected JSON object, got: {type(body)}"
    returned = body.get("result", body.get("value", body.get("data")))
    if isinstance(returned, dict) and "value" in returned:
        returned = returned["value"]
    assert returned == "rate_limit_name_2" or body.get("name") == "rate_limit_name_2", f"Expected Ok(Some(name)) on lock failure branch, got: {body}"

test_exc002_record_rate_limit_error_and_marked_cache_branches()