import requests

BASE_URL = "http://127.0.0.1:8080"

def _assert_json_like(obj):
    assert isinstance(obj, dict), f"Expected JSON object, got: {type(obj)}"

def _post_record_rate_limit(payload):
    resp = requests.post(f"{BASE_URL}/record_rate_limit", json=payload, timeout=10)
    assert resp.status_code == 200, f"Expected 200, got {resp.status_code}, body={resp.text}"
    data = resp.json()
    _assert_json_like(data)
    assert "ok" in data, f"Missing 'ok' field in response: {data}"
    return data

def _get_cache():
    resp = requests.get(f"{BASE_URL}/cache", timeout=10)
    assert resp.status_code == 200, f"Expected 200 from /cache, got {resp.status_code}, body={resp.text}"
    data = resp.json()
    _assert_json_like(data)
    assert "cache" in data, f"Missing 'cache' field in response: {data}"
    return data["cache"]

def _set_test_mode(mode, manager_result, poison_lock=False):
    payload = {
        "mode": mode,
        "manager_result": manager_result,
        "poison_lock": poison_lock
    }
    resp = requests.post(f"{BASE_URL}/test/setup", json=payload, timeout=10)
    assert resp.status_code == 200, f"/test/setup expected 200, got {resp.status_code}, body={resp.text}"
    data = resp.json()
    _assert_json_like(data)
    assert data.get("ok") is True, f"Setup failed: {data}"

def _seed_cache(value):
    resp = requests.post(f"{BASE_URL}/test/cache", json={"cache": value}, timeout=10)
    assert resp.status_code == 200, f"/test/cache expected 200, got {resp.status_code}, body={resp.text}"
    data = resp.json()
    _assert_json_like(data)
    assert data.get("ok") is True, f"Cache seed failed: {data}"

def test_bnd002_record_rate_limit_boundary_and_cache_clear_behavior():
    # Boundary payloads required by case
    boundary_inputs = [
        {"retry_after_secs": None, "message": ""},
        {"retry_after_secs": 0, "message": ""},
        {"retry_after_secs": 18446744073709551615, "message": "⏳限流"},
    ]

    # Subcase A: condition true, manager -> Some("acct"), healthy mutex => cache cleared (None)
    _set_test_mode(mode="live_injectable", manager_result="acct", poison_lock=False)
    _seed_cache({"preexisting": "value"})
    before = _get_cache()
    assert before is not None, f"Expected seeded cache before subcase A, got {before}"

    for payload in boundary_inputs:
        out = _post_record_rate_limit(payload)
        assert out["ok"] is True, f"Expected ok=True in subcase A, got {out}"
        assert out.get("marked") == "acct", f"Expected marked='acct' in subcase A, got {out}"
        cache_now = _get_cache()
        assert cache_now is None, f"Expected cache cleared to None in subcase A, got {cache_now}"
        _seed_cache({"preexisting": "value"})  # re-seed for next boundary input

    # Subcase B: marked is None => cache unchanged
    _set_test_mode(mode="live_injectable", manager_result=None, poison_lock=False)
    _seed_cache({"keep": "me"})
    before = _get_cache()
    assert before == {"keep": "me"}, f"Unexpected seeded cache before subcase B: {before}"

    for payload in boundary_inputs:
        out = _post_record_rate_limit(payload)
        assert out["ok"] is True, f"Expected ok=True in subcase B, got {out}"
        assert out.get("marked") is None, f"Expected marked=None in subcase B, got {out}"
        after = _get_cache()
        assert after == {"keep": "me"}, f"Expected cache unchanged in subcase B, got {after}"

    # Subcase C: lock failure (poisoned), manager -> Some(...), should still return Some and not clear cache
    _set_test_mode(mode="live_injectable", manager_result="acct_lock_fail", poison_lock=True)
    _seed_cache({"survive": True})
    before = _get_cache()
    assert before == {"survive": True}, f"Unexpected seeded cache before subcase C: {before}"

    for payload in boundary_inputs:
        out = _post_record_rate_limit(payload)
        assert out["ok"] is True, f"Expected ok=True in subcase C, got {out}"
        assert out.get("marked") == "acct_lock_fail", f"Expected marked='acct_lock_fail' in subcase C, got {out}"
        # lock failure should skip cache clear
        after = _get_cache()
        assert after == {"survive": True}, f"Expected cache clear skipped in subcase C, got {after}"

test_bnd002_record_rate_limit_boundary_and_cache_clear_behavior()