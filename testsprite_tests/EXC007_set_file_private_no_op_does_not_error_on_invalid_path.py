import requests

def test_set_file_private_noop_invalid_path():
    base_url = "http://127.0.0.1:8080"
    endpoint = f"{base_url}/set_file_private"

    payload = {
        "path": "/this/path/does/not/exist/and/should/be/inaccessible_EXC007"
    }

    resp = requests.post(endpoint, json=payload, timeout=10)

    # The core requirement is no panic/throw; service should return a normal HTTP response.
    assert resp.status_code < 500, f"Expected non-5xx response, got {resp.status_code}: {resp.text}"

    # Response should be JSON-shaped (or empty success body in some implementations).
    content_type = resp.headers.get("Content-Type", "")
    if resp.text.strip():
        assert "application/json" in content_type.lower(), (
            f"Expected JSON response when body is present, got Content-Type={content_type!r}"
        )
        data = resp.json()
        assert isinstance(data, dict), f"Expected JSON object, got: {type(data).__name__}"

        # Flexible shape checks: expect at least one common success/no-op indicator field.
        expected_keys = {"ok", "success", "status", "message", "error"}
        assert any(k in data for k in expected_keys), (
            f"Expected one of keys {expected_keys} in response JSON, got keys={set(data.keys())}"
        )

        # Should not indicate a hard server crash.
        if "status" in data and isinstance(data["status"], str):
            assert data["status"].lower() not in {"panic", "crash", "fatal"}, data
        if "error" in data and isinstance(data["error"], str):
            assert "panic" not in data["error"].lower(), data["error"]

test_set_file_private_noop_invalid_path()