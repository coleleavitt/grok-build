import requests

BASE_URL = "http://127.0.0.1:8080"


def test_bnd004_resolve_headers_maps_credential_and_propagates_error():
    # Success subcase: boundary-like minimal non-empty token
    success_payload = {
        "scenario": "resolve_headers",
        "mock": {
            "resolve_credential": {
                "result": {
                    "ok": {
                        "token": "a"
                    }
                }
            }
        },
        "input": {}
    }

    r = requests.post(f"{BASE_URL}/test/resolve_headers", json=success_payload, timeout=10)
    assert r.status_code == 200, f"Expected 200, got {r.status_code}: {r.text}"
    body = r.json()
    assert isinstance(body, dict), f"Expected JSON object, got: {body!r}"

    # Expected shape: success + produced header mutation equivalent to HeaderMutation::for_credential(credential)
    assert body.get("ok") is True or body.get("status") == "ok" or "result" in body, f"Unexpected success envelope: {body}"

    result = body.get("result", body)
    mutation = result.get("headerMutation") or result.get("header_mutation") or result.get("mutation")
    assert isinstance(mutation, dict), f"Expected header mutation object, got: {mutation!r}"

    # Accept common header naming variants
    headers = mutation.get("headers") or mutation.get("set") or mutation.get("add") or mutation
    assert isinstance(headers, dict), f"Expected headers map, got: {headers!r}"

    auth_val = headers.get("authorization") or headers.get("Authorization")
    assert isinstance(auth_val, str) and auth_val.strip(), f"Missing/invalid Authorization header in mutation: {headers}"

    # Ensure it was built from the credential token "a"
    assert "a" in auth_val, f"Authorization value does not include credential token: {auth_val!r}"

    # Error propagation subcase: manager resolve_credential returns Err; resolve_headers should return same error kind
    error_kind = "CredentialNotFound"
    error_payload = {
        "scenario": "resolve_headers",
        "mock": {
            "resolve_credential": {
                "result": {
                    "err": {
                        "kind": error_kind,
                        "message": "forced error for test"
                    }
                }
            }
        },
        "input": {}
    }

    r2 = requests.post(f"{BASE_URL}/test/resolve_headers", json=error_payload, timeout=10)
    assert r2.status_code in (400, 422, 500), f"Expected error status, got {r2.status_code}: {r2.text}"
    body2 = r2.json()
    assert isinstance(body2, dict), f"Expected JSON object, got: {body2!r}"

    err = body2.get("error") or body2.get("err") or body2
    assert isinstance(err, dict), f"Expected error object, got: {err!r}"

    returned_kind = err.get("kind") or err.get("errorKind") or err.get("type")
    assert returned_kind == error_kind, f"Expected error kind {error_kind!r}, got {returned_kind!r}. Full body: {body2}"


test_bnd004_resolve_headers_maps_credential_and_propagates_error()