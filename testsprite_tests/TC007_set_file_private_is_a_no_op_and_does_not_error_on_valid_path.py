import os
import tempfile
import requests

BASE_URL = "http://127.0.0.1:8080"

def test_tc007_set_file_private_noop_valid_path():
    with tempfile.NamedTemporaryFile(delete=False) as tmp:
        tmp.write(b"tc007-content")
        tmp_path = tmp.name

    try:
        # Attempt likely endpoint patterns to make the test resilient to minor routing differences.
        candidate_requests = [
            ("POST", f"{BASE_URL}/set_file_private", {"path": tmp_path}),
            ("POST", f"{BASE_URL}/file/set_private", {"path": tmp_path}),
            ("POST", f"{BASE_URL}/set-file-private", {"path": tmp_path}),
        ]

        response = None
        for method, url, payload in candidate_requests:
            r = requests.request(method, url, json=payload, timeout=10)
            if r.status_code != 404:
                response = r
                break

        assert response is not None, "Could not find set_file_private endpoint (all candidate routes returned 404)"
        assert response.status_code in (200, 204), f"Expected success status (200/204), got {response.status_code}"

        if response.status_code != 204:
            data = response.json()
            assert isinstance(data, dict), "Expected JSON object response"
            # Accept common success response shapes
            assert any(
                k in data for k in ("ok", "success", "status", "result")
            ), f"Unexpected response shape: {data}"

        # File should remain accessible (no-op behavior)
        assert os.path.exists(tmp_path), "Temporary file no longer exists after set_file_private"
        with open(tmp_path, "rb") as f:
            content = f.read()
        assert content == b"tc007-content", "File content changed unexpectedly"

    finally:
        if os.path.exists(tmp_path):
            os.remove(tmp_path)

test_tc007_set_file_private_noop_valid_path()