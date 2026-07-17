import requests
import time
import uuid

BASE_URL = "http://127.0.0.1:8080"


def _assert_json(resp):
    ctype = resp.headers.get("content-type", "")
    assert "application/json" in ctype.lower(), f"Expected JSON response, got Content-Type={ctype}"
    try:
        return resp.json()
    except Exception as e:
        raise AssertionError(f"Response is not valid JSON: {e}\nBody: {resp.text}")


def _request(method, path, **kwargs):
    url = f"{BASE_URL}{path}"
    resp = requests.request(method, url, timeout=10, **kwargs)
    return resp


def _upsert_account(name, unified_status="Ok"):
    payload = {"name": name, "unified_status": unified_status}
    resp = _request("POST", "/accounts", json=payload)
    assert resp.status_code in (200, 201), f"Upsert account failed: {resp.status_code} {resp.text}"
    body = _assert_json(resp)
    assert isinstance(body, dict), f"Expected object body, got: {type(body)}"
    assert body.get("name") == name, f"Expected name={name}, got: {body}"
    return body


def _set_selected(name_or_none):
    payload = {"selected": name_or_none}
    resp = _request("POST", "/selection", json=payload)
    assert resp.status_code in (200, 204), f"Set selection failed: {resp.status_code} {resp.text}"
    if resp.status_code == 200 and resp.text.strip():
        body = _assert_json(resp)
        assert isinstance(body, dict), f"Expected object body for selection set, got: {type(body)}"


def _get_account(name):
    resp = _request("GET", f"/accounts/{name}")
    if resp.status_code == 404:
        return None
    assert resp.status_code == 200, f"Get account failed: {resp.status_code} {resp.text}"
    body = _assert_json(resp)
    assert isinstance(body, dict), f"Expected object body, got: {type(body)}"
    assert body.get("name") == name, f"Expected name={name}, got: {body}"
    return body


def _list_accounts():
    resp = _request("GET", "/accounts")
    assert resp.status_code == 200, f"List accounts failed: {resp.status_code} {resp.text}"
    body = _assert_json(resp)
    assert isinstance(body, list), f"Expected list body, got: {type(body)}"
    return body


def _record_selected_rate_limit(message, retry_after_secs=None):
    payload = {"message": message}
    if retry_after_secs is not None:
        payload["retry_after_secs"] = retry_after_secs
    resp = _request("POST", "/auth/record_selected_rate_limit", json=payload)
    assert resp.status_code == 200, f"record_selected_rate_limit failed: {resp.status_code} {resp.text}"
    body = _assert_json(resp)
    assert isinstance(body, dict), f"Expected object body, got: {type(body)}"
    assert "selected_account" in body, f"Expected 'selected_account' field in response: {body}"
    return body


def test_bnd005_record_selected_rate_limit_boundary():
    suffix = uuid.uuid4().hex[:8]
    existing = f"acct_existing_{suffix}"
    missing = f"acct_missing_{suffix}"

    # Prepare a known existing account (Subcase A true branch target for find_mut)
    _upsert_account(existing, unified_status="Ok")

    # Case A1: retry_after_secs=None => default cooldown
    _set_selected(existing)
    before = int(time.time())
    out = _record_selected_rate_limit(message="")
    assert out["selected_account"] == existing, f"Expected selected_account={existing}, got {out}"

    acc = _get_account(existing)
    assert acc is not None, "Existing account should be present"
    assert acc.get("unified_status") == "Rejected", f"Expected unified_status=Rejected, got {acc}"
    assert acc.get("last_auth_error") == "", f"Expected last_auth_error='', got {acc}"

    rrt = acc.get("rate_limit_reset_time")
    assert rrt is not None, f"Expected rate_limit_reset_time to be set, got {acc}"
    assert isinstance(rrt, int), f"Expected integer rate_limit_reset_time, got {type(rrt)} value={rrt}"
    assert rrt >= before, f"Expected rate_limit_reset_time >= now({before}), got {rrt}"

    # Case A2: retry_after_secs=0 with long string message
    long_msg = "x" * 8192
    _set_selected(existing)
    now = int(time.time())
    out = _record_selected_rate_limit(message=long_msg, retry_after_secs=0)
    assert out["selected_account"] == existing, f"Expected selected_account={existing}, got {out}"

    acc = _get_account(existing)
    assert acc.get("unified_status") == "Rejected", f"Expected unified_status=Rejected after retry=0, got {acc}"
    assert acc.get("last_auth_error") == long_msg, "Expected last_auth_error to store long message"
    rrt0 = acc.get("rate_limit_reset_time")
    assert isinstance(rrt0, int), f"Expected integer rate_limit_reset_time, got {rrt0}"
    assert abs(rrt0 - now) <= 5, f"Expected near-now reset time for retry_after_secs=0, got {rrt0}, now={now}"

    # Case A3: retry_after_secs=u64::MAX equivalent => conversion failure fallback to default cooldown
    huge = 18446744073709551615
    unicode_msg = "错误🔒 — límite alcanzado — معدل محدود"
    _set_selected(existing)
    before_huge = int(time.time())
    out = _record_selected_rate_limit(message=unicode_msg, retry_after_secs=huge)
    assert out["selected_account"] == existing, f"Expected selected_account={existing}, got {out}"

    acc = _get_account(existing)
    assert acc.get("unified_status") == "Rejected", f"Expected unified_status=Rejected after huge retry, got {acc}"
    assert acc.get("last_auth_error") == unicode_msg, "Expected unicode message persisted as last_auth_error"
    rrth = acc.get("rate_limit_reset_time")
    assert isinstance(rrth, int), f"Expected integer rate_limit_reset_time, got {rrth}"
    assert rrth >= before_huge, f"Expected fallback/default reset time >= now({before_huge}), got {rrth}"
    assert rrth < 32503680000, f"Expected fallback/default cooldown, got implausibly huge timestamp: {rrth}"

    # Subcase B: selected name exists in selector but missing from data => returns None and no mutations
    _set_selected(missing)
    accounts_before = {a.get("name"): a for a in _list_accounts()}
    out = _record_selected_rate_limit(message="should not mutate", retry_after_secs=0)
    assert out["selected_account"] is None, f"Expected None when selected account missing, got {out}"

    accounts_after = {a.get("name"): a for a in _list_accounts()}
    assert accounts_before == accounts_after, "No account should be mutated when selected account not found"

    # select(None) path: no selected account => None
    _set_selected(None)
    out = _record_selected_rate_limit(message="no selection path")
    assert out["selected_account"] is None, f"Expected None when no selected account, got {out}"


test_bnd005_record_selected_rate_limit_boundary()