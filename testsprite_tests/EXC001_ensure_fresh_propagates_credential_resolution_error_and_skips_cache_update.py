import requests

BASE_URL = "http://127.0.0.1:8080"

def test_exc001_ensure_fresh_propagates_error_and_skips_cache_update():
    session = requests.Session()

    # 1) Pre-seed cache with known value
    preseed_payload = {
        "instance_id": "exc001-live-auth",
        "cache": {
            "access_token": "KNOWN_TOKEN",
            "expires_at": 4102444800
        }
    }
    r = session.post(f"{BASE_URL}/test/live-auth/cache/preseed", json=preseed_payload, timeout=10)
    assert r.status_code == 200, f"Expected 200 preseed, got {r.status_code}: {r.text}"
    body = r.json()
    assert isinstance(body, dict), "Preseed response must be JSON object"
    assert body.get("ok") is True, f"Preseed failed: {body}"

    # 2) Configure mocked manager to return resolve_credential error
    mock_payload = {
        "instance_id": "exc001-live-auth",
        "resolve_credential": {
            "result": "err",
            "error": "mocked credential resolution failure"
        }
    }
    r = session.post(f"{BASE_URL}/test/live-auth/mock-manager", json=mock_payload, timeout=10)
    assert r.status_code == 200, f"Expected 200 mock-manager, got {r.status_code}: {r.text}"
    body = r.json()
    assert isinstance(body, dict), "Mock-manager response must be JSON object"
    assert body.get("ok") is True, f"Mock setup failed: {body}"

    # 3) Call ensure_fresh and assert error (no panic => normal HTTP response)
    ensure_payload = {"instance_id": "exc001-live-auth"}
    r = session.post(f"{BASE_URL}/live-auth/ensure-fresh", json=ensure_payload, timeout=10)
    # For error propagation path, expect a non-2xx domain error, typically 400/422/500 depending on API contract.
    assert r.status_code in (400, 401, 403, 409, 422, 500), (
        f"Expected error status for resolve_credential failure, got {r.status_code}: {r.text}"
    )
    body = r.json()
    assert isinstance(body, dict), "ensure-fresh error response must be JSON object"
    assert body.get("ok") is False or "error" in body, f"Expected error-shaped body, got {body}"

    # 4) Verify cache remains unchanged
    r = session.get(f"{BASE_URL}/test/live-auth/cache", params={"instance_id": "exc001-live-auth"}, timeout=10)
    assert r.status_code == 200, f"Expected 200 cache read, got {r.status_code}: {r.text}"
    cache_body = r.json()
    assert isinstance(cache_body, dict), "Cache response must be JSON object"
    cache = cache_body.get("cache")
    assert isinstance(cache, dict), f"Expected cache object, got {cache_body}"
    assert cache.get("access_token") == "KNOWN_TOKEN", f"Cache token changed unexpectedly: {cache}"
    assert cache.get("expires_at") == 4102444800, f"Cache expiry changed unexpectedly: {cache}"

test_exc001_ensure_fresh_propagates_error_and_skips_cache_update()