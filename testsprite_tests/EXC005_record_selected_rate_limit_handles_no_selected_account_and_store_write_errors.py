import requests

BASE_URL = "http://127.0.0.1:8080"

def test_exc005_record_selected_rate_limit_branches():
    # Health check (service reachable)
    health = requests.get(f"{BASE_URL}/health", timeout=5)
    assert health.status_code in (200, 204, 404), f"Service not reachable as expected, got {health.status_code}"

    # 1) Missing selected account path: expect Ok(None)-like success with no record updated
    # Assumed test hook endpoint for this case.
    missing_payload = {
        "case": "record_selected_rate_limit",
        "store_mode": "selected_account_missing",
        "input": {
            "rejected": True,
            "rate_limit_reset_at": "2026-01-01T00:00:00Z"
        }
    }
    r_missing = requests.post(f"{BASE_URL}/test/EXC005", json=missing_payload, timeout=10)
    assert r_missing.status_code == 200, f"Expected 200 for missing-account path, got {r_missing.status_code}: {r_missing.text}"
    j_missing = r_missing.json()
    assert isinstance(j_missing, dict), "Response must be a JSON object"
    assert j_missing.get("ok") is True, f"Expected ok=True, got: {j_missing}"
    # Accept either explicit null result or semantic no-op markers
    assert (
        j_missing.get("result") is None
        or j_missing.get("updated") is False
        or j_missing.get("status") in ("none", "no_selected_account", "not_found")
    ), f"Expected Ok(None)-like outcome, got: {j_missing}"

    # 2) Write failure path: expect Err-like response
    write_fail_payload = {
        "case": "record_selected_rate_limit",
        "store_mode": "write_failure",
        "input": {
            "rejected": True,
            "rate_limit_reset_at": "2026-01-01T00:00:00Z"
        }
    }
    r_fail = requests.post(f"{BASE_URL}/test/EXC005", json=write_fail_payload, timeout=10)
    assert r_fail.status_code in (400, 409, 422, 500), f"Expected error status for write failure, got {r_fail.status_code}: {r_fail.text}"
    j_fail = r_fail.json()
    assert isinstance(j_fail, dict), "Error response must be a JSON object"
    assert j_fail.get("ok") is False or "error" in j_fail, f"Expected Err-like payload, got: {j_fail}"

    # 3) Account exists path: should update rejected/rate-limit fields (true branch)
    exists_payload = {
        "case": "record_selected_rate_limit",
        "store_mode": "selected_account_exists",
        "input": {
            "rejected": True,
            "rate_limit_reset_at": "2026-01-01T00:00:00Z"
        }
    }
    r_exists = requests.post(f"{BASE_URL}/test/EXC005", json=exists_payload, timeout=10)
    assert r_exists.status_code == 200, f"Expected 200 for existing-account path, got {r_exists.status_code}: {r_exists.text}"
    j_exists = r_exists.json()
    assert isinstance(j_exists, dict), "Response must be a JSON object"
    assert j_exists.get("ok") is True, f"Expected ok=True, got: {j_exists}"
    # Verify fields were set in returned account/object shape
    account = j_exists.get("account") or j_exists.get("result") or {}
    assert isinstance(account, dict), f"Expected account/result object, got: {account}"
    assert account.get("rejected") is True, f"Expected rejected=True in updated account, got: {account}"
    assert account.get("rate_limit_reset_at") == "2026-01-01T00:00:00Z", f"Expected rate_limit_reset_at set, got: {account}"

test_exc005_record_selected_rate_limit_branches()