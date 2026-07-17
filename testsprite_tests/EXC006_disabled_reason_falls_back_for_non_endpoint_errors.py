import requests

BASE_URL = "http://127.0.0.1:8080"

def test_exc006_disabled_reason_fallback_and_endpoint_code():
    # Try a few likely endpoint paths to remain robust across service routing styles.
    candidate_paths = [
        "/exceptions/disabled_reason",
        "/exception/disabled_reason",
        "/disabled_reason",
        "/v1/exceptions/disabled_reason",
        "/v1/exception/disabled_reason",
    ]

    test_payloads = [
        # Fallback path: non-endpoint-ish error (no oauth_error code)
        (
            {"error": {"type": "AnthropicAuthError", "variant": "NonEndpoint"}},
            "permanent_oauth_error",
            "fallback_non_endpoint",
        ),
        # Fallback path: endpoint variant but oauth_error is None/null
        (
            {"error": {"type": "AnthropicAuthError", "variant": "Endpoint", "oauth_error": None}},
            "permanent_oauth_error",
            "fallback_endpoint_none",
        ),
        # Endpoint with oauth_error present
        (
            {"error": {"type": "AnthropicAuthError", "variant": "Endpoint", "oauth_error": "invalid_grant"}},
            "invalid_grant",
            "endpoint_specific_code",
        ),
    ]

    last_err = None
    for path in candidate_paths:
        url = BASE_URL + path
        try:
            # Probe route existence with first payload
            probe_resp = requests.post(url, json=test_payloads[0][0], timeout=10)
            if probe_resp.status_code in (404, 405):
                continue

            # Run all assertions on this route
            for payload, expected_code, label in test_payloads:
                resp = requests.post(url, json=payload, timeout=10)
                assert resp.status_code == 200, f"{label}: expected 200, got {resp.status_code}, body={resp.text}"

                data = resp.json()
                assert isinstance(data, dict), f"{label}: expected JSON object, got {type(data).__name__}"

                # Accept common response shapes, but require exact expected value.
                if "disabled_reason" in data:
                    actual = data["disabled_reason"]
                elif "code" in data:
                    actual = data["code"]
                elif "result" in data:
                    actual = data["result"]
                elif "data" in data and isinstance(data["data"], dict):
                    nested = data["data"]
                    if "disabled_reason" in nested:
                        actual = nested["disabled_reason"]
                    elif "code" in nested:
                        actual = nested["code"]
                    elif "result" in nested:
                        actual = nested["result"]
                    else:
                        raise AssertionError(f"{label}: could not find expected key in nested data: {data}")
                else:
                    raise AssertionError(f"{label}: response shape missing expected key: {data}")

                assert isinstance(actual, str), f"{label}: expected string code, got {type(actual).__name__} ({actual})"
                assert actual == expected_code, f"{label}: expected '{expected_code}', got '{actual}'"

            return
        except Exception as e:
            last_err = e
            continue

    raise AssertionError(f"Could not find working disabled_reason endpoint under base URL {BASE_URL}. Last error: {last_err}")

test_exc006_disabled_reason_fallback_and_endpoint_code()