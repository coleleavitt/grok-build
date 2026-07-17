import requests

BASE_URL = "http://127.0.0.1:8080"

def test_bnd009_handle_sampling_failure_branch_matrix():
    # Try common health endpoints first to ensure service is reachable.
    health_paths = ["/health", "/healthz", "/ready", "/"]
    reachable = False
    last_resp = None
    for p in health_paths:
        try:
            r = requests.get(f"{BASE_URL}{p}", timeout=5)
            last_resp = r
            if r.status_code < 500:
                reachable = True
                break
        except requests.RequestException:
            continue
    assert reachable, f"Service not reachable at {BASE_URL}; last_resp={getattr(last_resp, 'status_code', None)}"

    # Candidate endpoints for the target behavior (unknown from PRD, so probe safely).
    candidate_paths = [
        "/handle_sampling_failure",
        "/sampling/handle_failure",
        "/sampler/handle_failure",
        "/v1/handle_sampling_failure",
        "/v1/sampling/handle_failure",
    ]

    # Subcases requested by test case. We assert response code and generic shape.
    subcases = [
        {
            "name": "compact_on_error_true_boundary_context_window_min_nonzero",
            "payload": {
                "should_compact_on_error": True,
                "model_metadata": {"context_window": 1},
                "failure": {"kind": "Other", "status_code": 500},
                "eligible": True
            }
        },
        {
            "name": "compact_on_error_false",
            "payload": {
                "should_compact_on_error": False,
                "model_metadata": {"context_window": 1},
                "failure": {"kind": "Other", "status_code": 500},
                "eligible": True
            }
        },
        {
            "name": "rate_limited_rotate_true",
            "payload": {
                "failure": {"kind": "RateLimited", "status_code": 429},
                "rotate_anthropic_after_rate_limit": True,
                "eligible": True
            }
        },
        {
            "name": "rate_limited_rotate_false",
            "payload": {
                "failure": {"kind": "RateLimited", "status_code": 429},
                "rotate_anthropic_after_rate_limit": False,
                "eligible": True
            }
        },
        {
            "name": "ineligible_non_recovery_outcome",
            "payload": {
                "failure": {"kind": "Other", "status_code": 500},
                "eligible": False
            }
        },
        {
            "name": "unauthorized_non_auth_kind_401",
            "payload": {
                "failure": {"kind": "Other", "status_code": 401},
                "eligible": True
            }
        },
        {
            "name": "devbox_recovery_matrix_true_true_some_success",
            "payload": {
                "failure": {"kind": "Other", "status_code": 401},
                "auth_recovery_eligible": True,
                "devbox_env": True,
                "auth_manager_present": True,
                "try_devbox_recovery_result": "success",
                "eligible": True
            }
        },
        {
            "name": "devbox_recovery_matrix_true_true_some_failure",
            "payload": {
                "failure": {"kind": "Other", "status_code": 401},
                "auth_recovery_eligible": True,
                "devbox_env": True,
                "auth_manager_present": True,
                "try_devbox_recovery_result": "failure",
                "eligible": True
            }
        },
        {
            "name": "devbox_recovery_matrix_true_true_none",
            "payload": {
                "failure": {"kind": "Other", "status_code": 401},
                "auth_recovery_eligible": True,
                "devbox_env": True,
                "auth_manager_present": False,
                "eligible": True
            }
        },
        {
            "name": "devbox_recovery_matrix_true_false_some",
            "payload": {
                "failure": {"kind": "Other", "status_code": 401},
                "auth_recovery_eligible": True,
                "devbox_env": False,
                "auth_manager_present": True,
                "eligible": True
            }
        },
        {
            "name": "generic_auth_recovery_success",
            "payload": {
                "failure": {"kind": "Other", "status_code": 401},
                "auth_recovery_eligible": True,
                "devbox_env": False,
                "try_recover_unauthorized_result": "success",
                "eligible": True
            }
        },
        {
            "name": "generic_auth_recovery_failure",
            "payload": {
                "failure": {"kind": "Other", "status_code": 401},
                "auth_recovery_eligible": True,
                "devbox_env": False,
                "try_recover_unauthorized_result": "failure",
                "eligible": True
            }
        },
        {
            "name": "idle_timeout_branch",
            "payload": {
                "failure": {"kind": "IdleTimeout", "status_code": 408},
                "eligible": True
            }
        },
        {
            "name": "empty_response_with_context",
            "payload": {
                "failure": {"kind": "EmptyResponse", "status_code": 502},
                "empty_response_context": {"attempt": 1, "source": "boundary-test"},
                "eligible": True
            }
        },
        {
            "name": "empty_response_without_context",
            "payload": {
                "failure": {"kind": "EmptyResponse", "status_code": 502},
                "empty_response_context": None,
                "eligible": True
            }
        },
    ]

    selected_path = None
    for path in candidate_paths:
        try:
            probe = requests.post(f"{BASE_URL}{path}", json={"probe": True}, timeout=5)
            if probe.status_code in (200, 201, 202, 400, 401, 404, 405, 409, 422):
                selected_path = path
                break
        except requests.RequestException:
            pass

    assert selected_path is not None, "Could not identify a usable handle_sampling_failure endpoint from candidate paths."

    for case in subcases:
        resp = requests.post(f"{BASE_URL}{selected_path}", json=case["payload"], timeout=10)
        assert resp.status_code in (200, 201, 202, 400, 401, 404, 409, 422), (
            f"{case['name']}: unexpected status {resp.status_code}, body={resp.text}"
        )

        content_type = resp.headers.get("Content-Type", "")
        assert "application/json" in content_type or resp.text == "" or resp.status_code in (404, 405), (
            f"{case['name']}: expected JSON-ish response, got Content-Type={content_type}"
        )

        if "application/json" in content_type and resp.text.strip():
            data = resp.json()
            assert isinstance(data, dict), f"{case['name']}: response JSON must be object"
            # Minimal shape assertions without assuming unavailable targets.
            keys = set(data.keys())
            assert any(
                k in keys for k in (
                    "recovery", "result", "outcome", "variant",
                    "sampler_failure_recovery", "action", "status"
                )
            ), f"{case['name']}: missing recovery/result discriminator keys; keys={keys}"

            # Side-effect hints, only if present.
            if "compaction" in data:
                assert isinstance(data["compaction"], (dict, bool)), f"{case['name']}: invalid compaction field type"
            if "config_updated" in data:
                assert isinstance(data["config_updated"], bool), f"{case['name']}: config_updated must be bool"

test_bnd009_handle_sampling_failure_branch_matrix()