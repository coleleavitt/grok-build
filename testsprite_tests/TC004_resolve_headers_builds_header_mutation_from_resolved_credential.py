import requests

BASE_URL = "http://127.0.0.1:8080"


def test_tc004_resolve_headers_builds_header_mutation_from_resolved_credential():
    """
    TC004:
    Use a Manager test double/spied instance where resolve_credential().await returns a known credential.
    Call resolve_headers().await and assert Ok(HeaderMutation::for_credential(known_credential))
    equivalent content (expected auth header fields).
    """

    # Arrange: known credential we expect the service to use for header mutation.
    known_credential = {
        "type": "bearer",
        "token": "known-test-token-123"
    }

    # This test assumes a debug/test-only endpoint exists to configure a spied manager double.
    # If unavailable, the assertions below will fail clearly.
    setup_resp = requests.post(
        f"{BASE_URL}/test-doubles/manager/spy",
        json={"resolve_credential_result": known_credential},
        timeout=10,
    )
    assert setup_resp.status_code == 200, (
        f"Failed to configure manager spy: {setup_resp.status_code} {setup_resp.text}"
    )
    setup_json = setup_resp.json()
    assert isinstance(setup_json, dict), "Spy setup response must be a JSON object"
    assert setup_json.get("ok") is True, "Spy setup must acknowledge success"

    # Act: call endpoint that triggers resolve_headers().await
    resp = requests.post(
        f"{BASE_URL}/manager/resolve-headers",
        timeout=10,
    )

    # Assert: successful result and header mutation equivalent to for_credential(known_credential)
    assert resp.status_code == 200, f"Unexpected status: {resp.status_code}, body={resp.text}"
    body = resp.json()
    assert isinstance(body, dict), "Response must be a JSON object"

    # Flexible shape handling: either direct mutation object or wrapped in {"ok": ...}
    payload = body.get("ok", body)
    assert isinstance(payload, dict), "Successful payload must be an object"

    # Expect header mutation content equivalent: Authorization: Bearer <token>
    # Accept multiple common shapes.
    if "headers" in payload and isinstance(payload["headers"], dict):
        headers = payload["headers"]
    elif "set" in payload and isinstance(payload["set"], dict):
        headers = payload["set"]
    else:
        headers = payload

    assert isinstance(headers, dict), "Header mutation must contain a header map"

    # Case-insensitive header key lookup
    auth_key = next((k for k in headers.keys() if k.lower() == "authorization"), None)
    assert auth_key is not None, f"Authorization header missing in mutation: {headers}"

    expected_auth = f"Bearer {known_credential['token']}"
    assert headers[auth_key] == expected_auth, (
        f"Authorization mismatch: expected '{expected_auth}', got '{headers[auth_key]}'"
    )

    # Optional spy verification endpoint to ensure async delegation happened exactly once.
    calls_resp = requests.get(
        f"{BASE_URL}/test-doubles/manager/spy/calls",
        timeout=10,
    )
    assert calls_resp.status_code == 200, (
        f"Failed to fetch spy calls: {calls_resp.status_code} {calls_resp.text}"
    )
    calls_json = calls_resp.json()
    assert isinstance(calls_json, dict), "Spy calls response must be object"
    resolve_credential_calls = calls_json.get("resolve_credential", 0)
    assert isinstance(resolve_credential_calls, int), "resolve_credential call count must be int"
    assert resolve_credential_calls >= 1, "Expected resolve_credential to be invoked at least once"


test_tc004_resolve_headers_builds_header_mutation_from_resolved_credential()