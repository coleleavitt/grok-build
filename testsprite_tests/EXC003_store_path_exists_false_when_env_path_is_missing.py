import requests

def test_store_path_exists_false_when_env_or_path_missing():
    base_url = "http://127.0.0.1:8080"

    # Try common candidate endpoints for store_path_exists
    candidates = [
        ("GET", "/store_path_exists"),
        ("GET", "/account/store_path_exists"),
        ("GET", "/accounts/store_path_exists"),
        ("POST", "/store_path_exists"),
        ("POST", "/account/store_path_exists"),
        ("POST", "/accounts/store_path_exists"),
    ]

    last_resp = None
    for method, path in candidates:
        url = base_url + path
        try:
            if method == "GET":
                resp = requests.get(url, timeout=5)
            else:
                resp = requests.post(url, timeout=5)
        except requests.RequestException:
            continue

        last_resp = resp
        if resp.status_code != 404:
            break

    assert last_resp is not None, "No response received from service"
    assert last_resp.status_code == 200, f"Expected 200, got {last_resp.status_code}: {last_resp.text}"

    try:
        body = last_resp.json()
    except ValueError as e:
        raise AssertionError(f"Response is not valid JSON: {last_resp.text}") from e

    assert isinstance(body, dict), f"Expected JSON object, got: {type(body).__name__}"

    # Accept a few common response shapes, but all must indicate false
    if "exists" in body:
        assert isinstance(body["exists"], bool), f"'exists' must be bool, got {type(body['exists']).__name__}"
        assert body["exists"] is False, f"Expected exists=false, got {body['exists']}"
    elif "store_path_exists" in body:
        assert isinstance(body["store_path_exists"], bool), (
            f"'store_path_exists' must be bool, got {type(body['store_path_exists']).__name__}"
        )
        assert body["store_path_exists"] is False, f"Expected store_path_exists=false, got {body['store_path_exists']}"
    elif "data" in body and isinstance(body["data"], dict) and "exists" in body["data"]:
        assert isinstance(body["data"]["exists"], bool), (
            f"'data.exists' must be bool, got {type(body['data']['exists']).__name__}"
        )
        assert body["data"]["exists"] is False, f"Expected data.exists=false, got {body['data']['exists']}"
    else:
        raise AssertionError(f"Unexpected response shape: {body}")

test_store_path_exists_false_when_env_or_path_missing()