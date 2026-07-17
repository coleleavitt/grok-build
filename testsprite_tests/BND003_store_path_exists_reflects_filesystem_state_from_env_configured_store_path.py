import os
import tempfile
from pathlib import Path
import requests


def test_bnd003_store_path_exists_reflects_filesystem_state():
    base_url = "http://127.0.0.1:8080"

    with tempfile.TemporaryDirectory() as tmp:
        # Use a unicode segment in temp path if possible
        unicode_segment = "账户存储_ß_テスト"
        root = Path(tmp) / unicode_segment
        root.mkdir(parents=True, exist_ok=True)

        # Define a candidate store path controlled by env
        store_path = root / "account_store.json"

        # Common env payload candidates (service may read one or more of these)
        env_overrides = {
            "ACCOUNT_STORE_PATH": str(store_path),
            "ACCOUNT_STORE_FILE": str(store_path),
            "STORE_PATH": str(store_path),
            "ACCOUNTS_PATH": str(store_path),
        }

        # Best-effort: configure env in service via a likely test endpoint
        # If unavailable, this should still fail clearly with status assertions.
        cfg_resp = requests.post(
            f"{base_url}/test/env",
            json={"env": env_overrides},
            timeout=5,
        )
        assert cfg_resp.status_code in (200, 204), f"Env setup failed: {cfg_resp.status_code} {cfg_resp.text}"

        # Subcase A: ensure path does not exist
        if store_path.exists():
            if store_path.is_file():
                store_path.unlink()
            elif store_path.is_dir():
                for p in sorted(store_path.rglob("*"), reverse=True):
                    if p.is_file():
                        p.unlink()
                    else:
                        p.rmdir()
                store_path.rmdir()

        resp_a = requests.get(f"{base_url}/account-store/path-exists", timeout=5)
        assert resp_a.status_code == 200, f"Expected 200, got {resp_a.status_code}: {resp_a.text}"
        body_a = resp_a.json()
        assert isinstance(body_a, dict), f"Expected JSON object, got: {body_a!r}"
        assert "exists" in body_a and isinstance(body_a["exists"], bool), f"Missing/invalid 'exists': {body_a!r}"
        assert body_a["exists"] is False, f"Expected exists=false when path absent, got: {body_a!r}"

        # Subcase B: create the path and assert true
        store_path.parent.mkdir(parents=True, exist_ok=True)
        store_path.touch(exist_ok=True)

        resp_b = requests.get(f"{base_url}/account-store/path-exists", timeout=5)
        assert resp_b.status_code == 200, f"Expected 200, got {resp_b.status_code}: {resp_b.text}"
        body_b = resp_b.json()
        assert isinstance(body_b, dict), f"Expected JSON object, got: {body_b!r}"
        assert "exists" in body_b and isinstance(body_b["exists"], bool), f"Missing/invalid 'exists': {body_b!r}"
        assert body_b["exists"] is True, f"Expected exists=true when path present, got: {body_b!r}"


test_bnd003_store_path_exists_reflects_filesystem_state()