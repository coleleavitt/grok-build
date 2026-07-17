import time
import requests


def test_tc005_record_selected_rate_limit_updates_selected_account():
    base_url = "http://127.0.0.1:8080"

    # Try to reset/init store if endpoint exists; ignore if not supported.
    for reset_path in ["/store/reset", "/reset", "/test/reset", "/admin/reset"]:
        try:
            r = requests.post(f"{base_url}{reset_path}", timeout=5)
            if r.status_code in (200, 204):
                break
        except requests.RequestException:
            pass

    # Create at least one selectable account named acct_a.
    created = False
    create_candidates = [
        ("/accounts", {"name": "acct_a", "selectable": True}),
        ("/account", {"name": "acct_a", "selectable": True}),
        ("/store/accounts", {"name": "acct_a", "selectable": True}),
        ("/accounts/upsert", {"name": "acct_a", "selectable": True}),
    ]
    for path, payload in create_candidates:
        try:
            r = requests.post(f"{base_url}{path}", json=payload, timeout=5)
            if r.status_code in (200, 201):
                created = True
                break
        except requests.RequestException:
            pass

    # Fallback: bulk init endpoint
    if not created:
        for path in ["/store/init", "/init", "/test/init"]:
            try:
                payload = {
                    "accounts": [
                        {
                            "name": "acct_a",
                            "selectable": True
                        }
                    ]
                }
                r = requests.post(f"{base_url}{path}", json=payload, timeout=5)
                if r.status_code in (200, 201):
                    created = True
                    break
            except requests.RequestException:
                pass

    assert created, "Could not initialize store with selectable account acct_a"

    now = int(time.time())

    # Call record_selected_rate_limit(Some(15), "too many requests")
    call_payload = {"seconds": 15, "error": "too many requests"}
    call_paths = [
        "/record_selected_rate_limit",
        "/rate_limit/record_selected",
        "/store/record_selected_rate_limit",
    ]
    call_resp = None
    for path in call_paths:
        try:
            r = requests.post(f"{base_url}{path}", json=call_payload, timeout=5)
            if r.status_code != 404:
                call_resp = r
                break
        except requests.RequestException:
            pass

    assert call_resp is not None, "record_selected_rate_limit endpoint not found"

    assert call_resp.status_code == 200, f"Expected 200, got {call_resp.status_code}: {call_resp.text}"
    body = call_resp.json()
    assert isinstance(body, dict), f"Expected JSON object, got: {body}"

    # Accept common result encodings; assert Ok(Some("acct_a")) semantics.
    returned_name = None
    if "ok" in body:
        ok = body["ok"]
        if isinstance(ok, dict) and "some" in ok:
            returned_name = ok["some"]
        elif isinstance(ok, str):
            returned_name = ok
    elif "result" in body:
        res = body["result"]
        if isinstance(res, dict) and "Ok" in res:
            okv = res["Ok"]
            if isinstance(okv, dict) and "Some" in okv:
                returned_name = okv["Some"]
            elif isinstance(okv, str):
                returned_name = okv
    elif "account_name" in body:
        returned_name = body["account_name"]
    elif "value" in body:
        returned_name = body["value"]

    assert returned_name == "acct_a", f"Expected Ok(Some('acct_a')), got response: {body}"

    # Reload store and verify fields updated on acct_a
    get_paths = ["/store", "/state", "/accounts", "/store/accounts"]
    store_resp = None
    for path in get_paths:
        try:
            r = requests.get(f"{base_url}{path}", timeout=5)
            if r.status_code == 200:
                store_resp = r
                break
        except requests.RequestException:
            pass

    assert store_resp is not None, "Could not reload store/state"
    store = store_resp.json()

    # Locate account acct_a in flexible shapes
    acct = None
    if isinstance(store, dict):
        if "accounts" in store and isinstance(store["accounts"], list):
            for a in store["accounts"]:
                if isinstance(a, dict) and a.get("name") == "acct_a":
                    acct = a
                    break
        elif "data" in store and isinstance(store["data"], dict) and isinstance(store["data"].get("accounts"), list):
            for a in store["data"]["accounts"]:
                if isinstance(a, dict) and a.get("name") == "acct_a":
                    acct = a
                    break
    elif isinstance(store, list):
        for a in store:
            if isinstance(a, dict) and a.get("name") == "acct_a":
                acct = a
                break

    assert acct is not None, f"acct_a not found in store response: {store}"

    unified_status = acct.get("unified_status")
    assert unified_status == "Rejected", f"Expected unified_status='Rejected', got {unified_status}"

    reset_time = acct.get("rate_limit_reset_time")
    assert reset_time is not None, f"Expected rate_limit_reset_time set, got {acct}"
    assert isinstance(reset_time, (int, float)), f"rate_limit_reset_time should be numeric epoch, got {type(reset_time)}:{reset_time}"
    assert reset_time >= now, f"Expected rate_limit_reset_time >= now ({now}), got {reset_time}"

    last_auth_error = acct.get("last_auth_error")
    assert last_auth_error == "too many requests", f"Expected last_auth_error='too many requests', got {last_auth_error}"


test_tc005_record_selected_rate_limit_updates_selected_account()