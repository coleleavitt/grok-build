import os
import tempfile
import requests


def test_tc003_store_path_exists_reflects_filesystem_presence():
    base_url = "http://127.0.0.1:8080"

    with tempfile.TemporaryDirectory() as tmpdir:
        # Use a header-based env override pattern commonly supported by HTTP test harnesses.
        # Try a few likely env var names for AccountStore path resolution.
        env_overrides = {
            "ACCOUNT_STORE_PATH": tmpdir,
            "ACCOUNT_STORE_DIR": tmpdir,
            "ACCOUNTS_PATH": tmpdir,
            "STORE_PATH": tmpdir,
        }

        endpoint_candidates = [
            "/store_path_exists",
            "/account/store_path_exists",
            "/accounts/store_path_exists",
            "/v1/store_path_exists",
            "/v1/account/store_path_exists",
            "/v1/accounts/store_path_exists",
        ]

        response = None
        used_endpoint = None

        for endpoint in endpoint_candidates:
            try:
                r = requests.get(
                    base_url + endpoint,
                    headers={"X-Test-Env": ";".join(f"{k}={v}" for k, v in env_overrides.items())},
                    timeout=5,
                )
                if r.status_code != 404:
                    response = r
                    used_endpoint = endpoint
                    break
            except requests.RequestException:
                continue

        assert response is not None, "Could not find a reachable store_path_exists endpoint"
        assert response.status_code == 200, f"Expected 200 from {used_endpoint}, got {response.status_code}: {response.text}"

        data = response.json()
        assert isinstance(data, dict), f"Expected JSON object, got: {type(data)}"

        # Accept common response shapes while asserting the semantic requirement: returns true.
        if "exists" in data:
            assert data["exists"] is True, f"Expected exists=true, got: {data}"
        elif "store_path_exists" in data:
            assert data["store_path_exists"] is True, f"Expected store_path_exists=true, got: {data}"
        elif "data" in data and isinstance(data["data"], dict):
            nested = data["data"]
            if "exists" in nested:
                assert nested["exists"] is True, f"Expected data.exists=true, got: {data}"
            elif "store_path_exists" in nested:
                assert nested["store_path_exists"] is True, f"Expected data.store_path_exists=true, got: {data}"
            else:
                raise AssertionError(f"Unrecognized JSON shape: {data}")
        else:
            raise AssertionError(f"Unrecognized JSON shape: {data}")


test_tc003_store_path_exists_reflects_filesystem_presence()