import requests

BASE_URL = "http://127.0.0.1:8080"

def test_bnd001_ensure_fresh_cache_and_poisoned_mutex_behavior():
    # Subcase A: healthy mutex lock path should cache credential
    payload_a = {
        "id": "BND001",
        "subcase": "A",
        "description": "healthy cache mutex; ensure_fresh should return mock credential and cache it"
    }
    resp_a = requests.post(f"{BASE_URL}/tests/run", json=payload_a, timeout=30)
    assert resp_a.status_code == 200, f"Subcase A expected 200, got {resp_a.status_code}: {resp_a.text}"
    data_a = resp_a.json()
    assert isinstance(data_a, dict), f"Subcase A response must be object, got: {type(data_a)}"
    assert data_a.get("id") == "BND001", f"Subcase A wrong id: {data_a}"
    assert data_a.get("subcase") == "A", f"Subcase A wrong subcase: {data_a}"
    assert data_a.get("ok") is True, f"Subcase A should pass: {data_a}"
    assert "credential" in data_a and isinstance(data_a["credential"], dict), f"Subcase A missing credential: {data_a}"
    assert "expected_credential" in data_a and isinstance(data_a["expected_credential"], dict), f"Subcase A missing expected_credential: {data_a}"
    assert data_a["credential"] == data_a["expected_credential"], f"Subcase A credential mismatch: {data_a}"
    cache_state_a = data_a.get("cache_state")
    assert isinstance(cache_state_a, dict), f"Subcase A missing cache_state object: {data_a}"
    assert cache_state_a.get("is_some") is True, f"Subcase A cache should contain Some(cloned credential): {data_a}"
    assert cache_state_a.get("value") == data_a["expected_credential"], f"Subcase A cached value mismatch: {data_a}"

    # Subcase B: poisoned mutex lock path should still return credential and not panic
    payload_b = {
        "id": "BND001",
        "subcase": "B",
        "description": "poisoned cache mutex; ensure_fresh should still return credential and not panic"
    }
    resp_b = requests.post(f"{BASE_URL}/tests/run", json=payload_b, timeout=30)
    assert resp_b.status_code == 200, f"Subcase B expected 200, got {resp_b.status_code}: {resp_b.text}"
    data_b = resp_b.json()
    assert isinstance(data_b, dict), f"Subcase B response must be object, got: {type(data_b)}"
    assert data_b.get("id") == "BND001", f"Subcase B wrong id: {data_b}"
    assert data_b.get("subcase") == "B", f"Subcase B wrong subcase: {data_b}"
    assert data_b.get("ok") is True, f"Subcase B should pass: {data_b}"
    assert data_b.get("panicked") is False, f"Subcase B should not panic: {data_b}"
    assert "credential" in data_b and isinstance(data_b["credential"], dict), f"Subcase B missing credential: {data_b}"
    assert "expected_credential" in data_b and isinstance(data_b["expected_credential"], dict), f"Subcase B missing expected_credential: {data_b}"
    assert data_b["credential"] == data_b["expected_credential"], f"Subcase B credential mismatch: {data_b}"
    cache_state_b = data_b.get("cache_state")
    assert isinstance(cache_state_b, dict), f"Subcase B missing cache_state object: {data_b}"
    assert cache_state_b.get("poisoned") is True, f"Subcase B should report poisoned mutex: {data_b}"
    assert cache_state_b.get("write_skipped") is True, f"Subcase B should skip cache write when lock fails: {data_b}"

test_bnd001_ensure_fresh_cache_and_poisoned_mutex_behavior()