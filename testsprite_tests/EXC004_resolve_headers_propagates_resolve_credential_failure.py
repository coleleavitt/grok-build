import requests

BASE_URL = "http://127.0.0.1:8080"


def _assert_err_shape(payload):
    assert isinstance(payload, dict), f"Expected JSON object, got: {type(payload)}"
    assert "ok" in payload, f"Missing 'ok' field: {payload}"
    assert payload["ok"] is False, f"Expected ok=False for error case, got: {payload.get('ok')}"
    assert "error" in payload, f"Missing 'error' field in error response: {payload}"
    assert payload["error"] is not None, f"Expected non-null error details: {payload}"


def test_exc004_resolve_headers_propagates_resolve_credential_failure():
    # Failure path: simulate manager.resolve_credential() failure.
    # We try a set of likely fault-injection keys so the test can work with varying API contracts.
    failure_payload_candidates = [
        {
            "headers": {"X-Existing": "keep-me"},
            "credential_ref": "acct:broken",
            "inject": {"resolve_credential": "fail"},
        },
        {
            "headers": {"X-Existing": "keep-me"},
            "credential_ref": "acct:broken",
            "mock": {"resolve_credential_error": "auth unavailable"},
        },
        {
            "headers": {"X-Existing": "keep-me"},
            "credential_ref": "acct:broken",
            "test_flags": {"force_resolve_credential_failure": True},
        },
    ]

    failure_response = None
    used_payload = None
    for candidate in failure_payload_candidates:
        r = requests.post(f"{BASE_URL}/resolve_headers", json=candidate, timeout=10)
        # Accept first contract that appears to be handled endpoint (not pure 404/405)
        if r.status_code not in (404, 405):
            failure_response = r
            used_payload = candidate
            break

    assert failure_response is not None, "resolve_headers endpoint not found or method unsupported on all attempts"
    assert failure_response.status_code in (200, 400, 422, 500), (
        f"Unexpected status for failure path: {failure_response.status_code} body={failure_response.text}"
    )

    failure_json = failure_response.json()
    _assert_err_shape(failure_json)

    # Assert no header mutation is produced on error.
    # We validate common shapes and ensure no derived auth header appears.
    forbidden_keys = {"authorization", "x-api-key", "proxy-authorization"}
    if "headers" in failure_json and isinstance(failure_json["headers"], dict):
        lowered = {k.lower() for k in failure_json["headers"].keys()}
        assert lowered.isdisjoint(forbidden_keys), (
            f"Error response should not include mutated credential headers: {failure_json['headers']}"
        )
        # If headers are echoed, ensure original known header remains unmodified.
        if "X-Existing" in used_payload.get("headers", {}):
            assert failure_json["headers"].get("X-Existing") in ("keep-me", None)

    # Optional success control subcheck: valid credential branch still works.
    success_payload_candidates = [
        {
            "headers": {"X-Existing": "keep-me"},
            "credential_ref": "acct:valid",
            "mock": {"resolve_credential": {"type": "bearer", "token": "abc123"}},
        },
        {
            "headers": {"X-Existing": "keep-me"},
            "credential_ref": "acct:valid",
            "test_flags": {"force_resolve_credential_success": True},
            "resolved_credential": {"type": "bearer", "token": "abc123"},
        },
    ]

    success_response = None
    for candidate in success_payload_candidates:
        r = requests.post(f"{BASE_URL}/resolve_headers", json=candidate, timeout=10)
        if r.status_code in (200, 201):
            success_response = r
            break

    if success_response is not None:
        body = success_response.json()
        assert isinstance(body, dict), f"Expected JSON object for success response, got {type(body)}"
        assert body.get("ok") is True, f"Expected ok=True for success control, got: {body}"
        assert "headers" in body and isinstance(body["headers"], dict), f"Missing/invalid headers in success: {body}"
        out_headers_lower = {k.lower(): v for k, v in body["headers"].items()}
        assert any(k in out_headers_lower for k in ("authorization", "x-api-key", "proxy-authorization")), (
            f"Expected credential-derived header mutation on success branch, got: {body['headers']}"
        )


test_exc004_resolve_headers_propagates_resolve_credential_failure()