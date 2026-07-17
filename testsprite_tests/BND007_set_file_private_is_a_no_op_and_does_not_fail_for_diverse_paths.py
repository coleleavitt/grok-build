import requests

def test_bnd007_set_file_private_boundary_paths():
    base_url = "http://127.0.0.1:8080"

    candidates = [
        "/set_file_private",                 # common REST style
        "/api/set_file_private",             # namespaced
        "/rpc/set_file_private",             # rpc style
        "/v1/set_file_private",              # versioned
    ]

    payloads = [
        {"path": "/tmp/existing_file.txt"},   # existing file path candidate
        {"path": "/tmp/does_not_exist_1234567890.txt"},  # non-existent
        {"path": "/tmp/ユニコード_文件_🔒.txt"},        # unicode filename
    ]

    last_error = None
    successful_endpoint = None

    # Find a working endpoint shape once, then reuse for all boundary payloads.
    for ep in candidates:
        try:
            r = requests.post(base_url + ep, json=payloads[0], timeout=5)
            if r.status_code in (200, 201, 202, 204):
                successful_endpoint = ep
                break
            # Accept method mismatch fallback check
            if r.status_code == 405:
                r2 = requests.get(base_url + ep, params=payloads[0], timeout=5)
                if r2.status_code in (200, 201, 202, 204):
                    successful_endpoint = ep
                    break
                last_error = f"{ep} GET -> {r2.status_code}, body={r2.text[:300]}"
            else:
                last_error = f"{ep} POST -> {r.status_code}, body={r.text[:300]}"
        except Exception as e:
            last_error = f"{ep} exception: {e}"

    assert successful_endpoint is not None, f"Could not locate set_file_private endpoint. Last error: {last_error}"

    for p in payloads:
        # Prefer POST with JSON; fallback to GET with query params on 405.
        resp = requests.post(base_url + successful_endpoint, json=p, timeout=5)
        if resp.status_code == 405:
            resp = requests.get(base_url + successful_endpoint, params=p, timeout=5)

        assert resp.status_code in (200, 201, 202, 204), (
            f"Expected success for path={p['path']!r}, got {resp.status_code}, body={resp.text[:500]}"
        )

        # Unit-like/no-op response shape assertions (non-strict, platform-independent)
        if resp.status_code != 204:
            ctype = (resp.headers.get("Content-Type") or "").lower()
            if "application/json" in ctype:
                body = resp.json()
                assert body is None or isinstance(body, (dict, list, bool, str, int, float)), (
                    f"Unexpected JSON shape for path={p['path']!r}: {type(body).__name__}"
                )
            else:
                # For non-JSON, allow empty or textual acknowledgements.
                assert isinstance(resp.text, str)

test_bnd007_set_file_private_boundary_paths()