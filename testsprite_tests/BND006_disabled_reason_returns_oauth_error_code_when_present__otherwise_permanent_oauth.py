import requests

def test_bnd006_disabled_reason_variants():
    base_url = "http://127.0.0.1:8080"

    # Subcase A1: Endpoint with oauth_error = "invalid_grant" => exact returned string
    payload_a1 = {
        "variant": "Endpoint",
        "oauth_error": "invalid_grant"
    }
    r_a1 = requests.post(f"{base_url}/anthropic_auth_error/disabled_reason", json=payload_a1, timeout=10)
    assert r_a1.status_code == 200, f"A1 expected 200, got {r_a1.status_code}: {r_a1.text}"
    body_a1 = r_a1.json()
    assert isinstance(body_a1, dict), f"A1 expected JSON object, got: {body_a1!r}"
    assert "disabled_reason" in body_a1, f"A1 missing 'disabled_reason': {body_a1}"
    assert body_a1["disabled_reason"] == "invalid_grant", f"A1 expected 'invalid_grant', got {body_a1['disabled_reason']!r}"

    # Subcase A2: Endpoint with unicode oauth_error boundary => exact returned unicode
    payload_a2 = {
        "variant": "Endpoint",
        "oauth_error": "错误"
    }
    r_a2 = requests.post(f"{base_url}/anthropic_auth_error/disabled_reason", json=payload_a2, timeout=10)
    assert r_a2.status_code == 200, f"A2 expected 200, got {r_a2.status_code}: {r_a2.text}"
    body_a2 = r_a2.json()
    assert isinstance(body_a2, dict), f"A2 expected JSON object, got: {body_a2!r}"
    assert "disabled_reason" in body_a2, f"A2 missing 'disabled_reason': {body_a2}"
    assert body_a2["disabled_reason"] == "错误", f"A2 expected '错误', got {body_a2['disabled_reason']!r}"

    # Subcase B: Endpoint with oauth_error = None => "permanent_oauth_error"
    payload_b = {
        "variant": "Endpoint",
        "oauth_error": None
    }
    r_b = requests.post(f"{base_url}/anthropic_auth_error/disabled_reason", json=payload_b, timeout=10)
    assert r_b.status_code == 200, f"B expected 200, got {r_b.status_code}: {r_b.text}"
    body_b = r_b.json()
    assert isinstance(body_b, dict), f"B expected JSON object, got: {body_b!r}"
    assert "disabled_reason" in body_b, f"B missing 'disabled_reason': {body_b}"
    assert body_b["disabled_reason"] == "permanent_oauth_error", (
        f"B expected 'permanent_oauth_error', got {body_b['disabled_reason']!r}"
    )

    # Subcase C: non-Endpoint variant => "permanent_oauth_error"
    payload_c = {
        "variant": "Other"
    }
    r_c = requests.post(f"{base_url}/anthropic_auth_error/disabled_reason", json=payload_c, timeout=10)
    assert r_c.status_code == 200, f"C expected 200, got {r_c.status_code}: {r_c.text}"
    body_c = r_c.json()
    assert isinstance(body_c, dict), f"C expected JSON object, got: {body_c!r}"
    assert "disabled_reason" in body_c, f"C missing 'disabled_reason': {body_c}"
    assert body_c["disabled_reason"] == "permanent_oauth_error", (
        f"C expected 'permanent_oauth_error', got {body_c['disabled_reason']!r}"
    )

test_bnd006_disabled_reason_variants()