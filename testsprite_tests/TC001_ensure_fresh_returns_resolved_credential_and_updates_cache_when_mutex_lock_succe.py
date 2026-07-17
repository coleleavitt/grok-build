import requests

def test_tc001_ensure_fresh_returns_resolved_credential_and_updates_cache():
    base_url = "http://127.0.0.1:8080"

    # Create a Live auth instance with a mock manager configured to return a known credential.
    # Assumed test endpoint contract:
    # POST /test/live-auth-instances
    # body: {
    #   "manager": {"type":"mock","resolved_credential":{"value":"known-credential"}},
    #   "cache": {"poisoned": false}
    # }
    create_payload = {
        "manager": {
            "type": "mock",
            "resolved_credential": {"value": "known-credential"}
        },
        "cache": {
            "poisoned": False
        }
    }

    create_resp = requests.post(f"{base_url}/test/live-auth-instances", json=create_payload, timeout=10)
    assert create_resp.status_code == 201, f"Expected 201 creating instance, got {create_resp.status_code}: {create_resp.text}"
    create_body = create_resp.json()
    assert isinstance(create_body, dict), f"Create response must be object, got: {create_body}"
    assert "id" in create_body and isinstance(create_body["id"], str) and create_body["id"], f"Missing/invalid id: {create_body}"
    instance_id = create_body["id"]

    # Call ensure_fresh()
    # Assumed test endpoint contract:
    # POST /test/live-auth-instances/{id}/ensure-fresh
    ensure_resp = requests.post(f"{base_url}/test/live-auth-instances/{instance_id}/ensure-fresh", timeout=10)
    assert ensure_resp.status_code == 200, f"Expected 200 from ensure_fresh, got {ensure_resp.status_code}: {ensure_resp.text}"
    ensure_body = ensure_resp.json()
    assert isinstance(ensure_body, dict), f"ensure_fresh response must be object, got: {ensure_body}"

    # Assert Result is Ok with the same credential value
    # Expected shape: {"result":{"Ok":{"value":"known-credential"}}}
    assert "result" in ensure_body and isinstance(ensure_body["result"], dict), f"Missing/invalid result field: {ensure_body}"
    assert "Ok" in ensure_body["result"], f"Expected Ok result, got: {ensure_body['result']}"
    ok_val = ensure_body["result"]["Ok"]
    assert isinstance(ok_val, dict), f"Ok payload must be object, got: {ok_val}"
    assert ok_val.get("value") == "known-credential", f"Unexpected credential value: {ok_val}"

    # Assert cache now contains Some(cloned_credential)
    # Assumed test endpoint contract:
    # GET /test/live-auth-instances/{id}/cache
    # returns {"cache":{"Some":{"value":"known-credential"}}}
    cache_resp = requests.get(f"{base_url}/test/live-auth-instances/{instance_id}/cache", timeout=10)
    assert cache_resp.status_code == 200, f"Expected 200 reading cache, got {cache_resp.status_code}: {cache_resp.text}"
    cache_body = cache_resp.json()
    assert isinstance(cache_body, dict), f"Cache response must be object, got: {cache_body}"
    assert "cache" in cache_body and isinstance(cache_body["cache"], dict), f"Missing/invalid cache field: {cache_body}"
    assert "Some" in cache_body["cache"], f"Expected cache to be Some(...), got: {cache_body['cache']}"
    some_val = cache_body["cache"]["Some"]
    assert isinstance(some_val, dict), f"Some payload must be object, got: {some_val}"
    assert some_val.get("value") == "known-credential", f"Cached credential mismatch: {some_val}"

test_tc001_ensure_fresh_returns_resolved_credential_and_updates_cache()