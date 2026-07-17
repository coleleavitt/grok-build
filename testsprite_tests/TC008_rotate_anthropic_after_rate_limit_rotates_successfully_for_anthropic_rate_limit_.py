import requests

BASE_URL = "http://127.0.0.1:8080"

def test_tc008_rotate_anthropic_after_rate_limit_success():
    # Step 1: Set up session state with sampling config and anthropic adapter recognized
    setup_payload = {
        "sampling_config": {
            "enabled": True,
            "temperature": 0.7,
            "top_p": 0.9
        },
        "provider_request_adapter": "anthropic",
        "credential_service_mock": {
            "record_rate_limit_result": {"ok": True, "value": "acct_a"},
            "has_usable_accounts": True,
            "ensure_fresh_result": {"ok": True, "credential": {"api_key": "anthropic_test_key"}}
        },
        "spy": {
            "prepare_sampler_for_turn": True
        }
    }

    setup_resp = requests.post(f"{BASE_URL}/test/session/setup", json=setup_payload, timeout=10)
    assert setup_resp.status_code == 200, f"Expected 200 from setup, got {setup_resp.status_code}: {setup_resp.text}"
    setup_json = setup_resp.json()
    assert isinstance(setup_json, dict), "Setup response must be a JSON object"
    assert setup_json.get("ok") is True, f"Setup did not return ok=true: {setup_json}"

    # Step 2: Call rotate_anthropic_after_rate_limit with an error containing retry_after and message
    rotate_payload = {
        "error": {
            "type": "rate_limit_error",
            "message": "Rate limit exceeded",
            "retry_after": 2
        }
    }

    rotate_resp = requests.post(f"{BASE_URL}/test/rotate_anthropic_after_rate_limit", json=rotate_payload, timeout=10)
    assert rotate_resp.status_code == 200, f"Expected 200 from rotate call, got {rotate_resp.status_code}: {rotate_resp.text}"
    rotate_json = rotate_resp.json()
    assert isinstance(rotate_json, dict), "Rotate response must be a JSON object"

    # Step 3: Assert result is true (happy path)
    assert "result" in rotate_json, f"Missing 'result' in rotate response: {rotate_json}"
    assert rotate_json["result"] is True, f"Expected rotate result=true, got: {rotate_json}"

    # Step 4: Verify prepare_sampler_for_turn invoked once
    verify_resp = requests.get(f"{BASE_URL}/test/spies/prepare_sampler_for_turn", timeout=10)
    assert verify_resp.status_code == 200, f"Expected 200 from spy endpoint, got {verify_resp.status_code}: {verify_resp.text}"
    verify_json = verify_resp.json()
    assert isinstance(verify_json, dict), "Spy response must be a JSON object"
    assert "call_count" in verify_json, f"Missing 'call_count' in spy response: {verify_json}"
    assert verify_json["call_count"] == 1, f"Expected prepare_sampler_for_turn call_count=1, got {verify_json['call_count']}"

test_tc008_rotate_anthropic_after_rate_limit_success()