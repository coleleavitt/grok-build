import requests

def test_tc006_disabled_reason_oauth_error_mapping():
    base_url = "http://127.0.0.1:8080"
    url = f"{base_url}/disabled_reason"

    payload = {
        "error": {
            "type": "Endpoint",
            "oauth_error": "invalid_grant"
        }
    }

    response = requests.post(url, json=payload, timeout=10)
    assert response.status_code == 200, f"Expected 200, got {response.status_code}: {response.text}"

    data = response.json()
    assert isinstance(data, dict), f"Expected JSON object, got: {type(data)}"
    assert "disabled_reason" in data, f"Missing 'disabled_reason' in response: {data}"
    assert data["disabled_reason"] == "invalid_grant", (
        f"Expected disabled_reason 'invalid_grant', got: {data['disabled_reason']}"
    )

test_tc006_disabled_reason_oauth_error_mapping()