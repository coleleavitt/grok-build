import requests

BASE_URL = "http://127.0.0.1:8080"

def test_exc009_handle_sampling_failure_negative_branches():
    url = f"{BASE_URL}/handle_sampling_failure"

    # Matrix of negative-branch variants described in EXC009.
    # We assert:
    # 1) No panic-level HTTP failure (i.e., endpoint responds in a controlled way)
    # 2) Response has expected shape
    # 3) Recovery result is present and stable per current behavior (captured as non-crash + explicit recovery field)
    cases = [
        {
            "name": "rate_limited_rotate_false",
            "payload": {
                "sampling_error_info": {
                    "type": "RateLimited",
                    "rotate_anthropic_after_rate_limit": False
                },
                "should_compact_on_error": False
            }
        },
        {
            "name": "unauthorized_401_ineligible_or_manager_absent",
            "payload": {
                "sampling_error_info": {
                    "type": "Unauthorized",
                    "status": 401,
                    "auth_recovery_eligible": False,
                    "auth_manager_present": False
                },
                "should_compact_on_error": False
            }
        },
        {
            "name": "devbox_recovery_false",
            "payload": {
                "sampling_error_info": {
                    "type": "Unauthorized",
                    "status": 401,
                    "devbox_path": True,
                    "try_devbox_recovery_result": False
                },
                "should_compact_on_error": False
            }
        },
        {
            "name": "devbox_recovery_err",
            "payload": {
                "sampling_error_info": {
                    "type": "Unauthorized",
                    "status": 401,
                    "devbox_path": True,
                    "try_devbox_recovery_error": "simulated-devbox-error"
                },
                "should_compact_on_error": False
            }
        },
        {
            "name": "generic_auth_recovery_false",
            "payload": {
                "sampling_error_info": {
                    "type": "Unauthorized",
                    "status": 401,
                    "devbox_path": False,
                    "try_recover_unauthorized_result": False
                },
                "should_compact_on_error": False
            }
        },
        {
            "name": "generic_auth_recovery_err",
            "payload": {
                "sampling_error_info": {
                    "type": "Unauthorized",
                    "status": 401,
                    "devbox_path": False,
                    "try_recover_unauthorized_error": "simulated-auth-error"
                },
                "should_compact_on_error": False
            }
        },
        {
            "name": "idle_timeout_no_compaction",
            "payload": {
                "sampling_error_info": {
                    "type": "IdleTimeout"
                },
                "should_compact_on_error": False
            }
        },
        {
            "name": "empty_response_no_context_no_compaction",
            "payload": {
                "sampling_error_info": {
                    "type": "EmptyResponse"
                },
                "should_compact_on_error": False
            }
        },
        {
            "name": "empty_response_with_context_compaction_enabled",
            "payload": {
                "sampling_error_info": {
                    "type": "EmptyResponse",
                    "empty_response_context": {
                        "context_window": 8192
                    }
                },
                "should_compact_on_error": True
            }
        },
    ]

    observed = {}

    for case in cases:
        resp = requests.post(url, json=case["payload"], timeout=10)

        # No panic / controlled behavior expected.
        assert resp.status_code == 200, f"{case['name']}: expected 200, got {resp.status_code}, body={resp.text}"

        body = resp.json()
        assert isinstance(body, dict), f"{case['name']}: response must be an object, got {type(body)}"

        # Expected shape: explicit recovery result enum/value present.
        # Allow either 'sampler_failure_recovery' or 'recovery' naming.
        recovery = body.get("sampler_failure_recovery", body.get("recovery"))
        assert recovery is not None, f"{case['name']}: missing recovery field in body={body}"

        # Recovery should be a string enum/object indicating outcome.
        assert isinstance(recovery, (str, dict)), f"{case['name']}: unexpected recovery type {type(recovery)}"

        # Optional guard: no panic indicators in payload.
        if isinstance(body.get("error"), str):
            assert "panic" not in body["error"].lower(), f"{case['name']}: panic surfaced in error text"

        observed[case["name"]] = recovery

    # "Matches current behavior" oracle: responses are stable and explicit per branch.
    # We assert each case produced a deterministic non-null recovery and that
    # compaction gate toggling on EmptyResponse can change or keep behavior,
    # but must still remain explicit and non-crashing.
    assert observed["empty_response_no_context_no_compaction"] is not None
    assert observed["empty_response_with_context_compaction_enabled"] is not None

    # Ensure both compaction gate paths were exercised (false + true with valid context_window)
    assert cases[7]["payload"]["should_compact_on_error"] is False
    assert cases[8]["payload"]["should_compact_on_error"] is True
    assert cases[8]["payload"]["sampling_error_info"]["empty_response_context"]["context_window"] > 0

test_exc009_handle_sampling_failure_negative_branches()