import requests

BASE_URL = "http://127.0.0.1:8080"

def _post_json(path, payload):
    r = requests.post(f"{BASE_URL}{path}", json=payload, timeout=10)
    try:
        body = r.json()
    except Exception:
        body = None
    return r, body

def _assert_common_shape(body):
    assert isinstance(body, dict), f"Expected JSON object, got: {type(body)} / {body}"
    assert "result" in body, f"Missing 'result' in response: {body}"
    assert isinstance(body["result"], bool), f"'result' must be bool, got: {body['result']!r}"

def test_bnd008_rotate_anthropic_after_rate_limit():
    """
    Boundary-focused subcases for:
    rotate_anthropic_after_rate_limit returns true only on anthropic + successful mark + usable accounts + fresh credential.
    """

    # Probe candidate endpoints to accommodate harness routing differences.
    candidate_paths = [
        "/test/rotate_anthropic_after_rate_limit",
        "/tests/rotate_anthropic_after_rate_limit",
        "/harness/rotate_anthropic_after_rate_limit",
        "/session/rotate_anthropic_after_rate_limit",
        "/rotate_anthropic_after_rate_limit",
    ]

    subcases = [
        {
            "name": "1_sampling_config_none_false",
            "payload": {
                "sampling_config": None,
                "adapter": "anthropic",
                "record_rate_limit": {"ok": {"retry_after_secs": None}},
                "has_usable_accounts": True,
                "ensure_fresh": {"ok": True},
                "error": {"message": "レート制限 exceeded 🚦"},
                "expected_prepare_sampler_for_turn_calls": 0,
            },
            "expected_result": False,
        },
        {
            "name": "2_non_anthropic_false",
            "payload": {
                "sampling_config": {"temperature": 0.2},
                "adapter": "openai",
                "record_rate_limit": {"ok": {"retry_after_secs": None}},
                "has_usable_accounts": True,
                "ensure_fresh": {"ok": True},
                "error": {"message": "错误: rate limit"},
                "expected_prepare_sampler_for_turn_calls": 0,
            },
            "expected_result": False,
        },
        {
            "name": "3_record_ok_none_false",
            "payload": {
                "sampling_config": {"temperature": 0.2},
                "adapter": "anthropic",
                "record_rate_limit": {"ok": None},
                "has_usable_accounts": True,
                "ensure_fresh": {"ok": True},
                "error": {"message": "лимит достигнут"},
                "expected_prepare_sampler_for_turn_calls": 0,
            },
            "expected_result": False,
        },
        {
            "name": "4_record_err_false",
            "payload": {
                "sampling_config": {"temperature": 0.2},
                "adapter": "anthropic",
                "record_rate_limit": {"err": {"message": "db write failed 💥"}},
                "has_usable_accounts": True,
                "ensure_fresh": {"ok": True},
                "error": {"message": "त्रुटि: सीमा"},
                "expected_prepare_sampler_for_turn_calls": 0,
            },
            "expected_result": False,
        },
        {
            "name": "5_has_usable_accounts_false",
            "payload": {
                "sampling_config": {"temperature": 0.2},
                "adapter": "anthropic",
                "record_rate_limit": {"ok": {"retry_after_secs": 0}},
                "has_usable_accounts": False,
                "ensure_fresh": {"ok": True},
                "error": {"message": "حد المعدل"},
                "expected_prepare_sampler_for_turn_calls": 0,
            },
            "expected_result": False,
        },
        {
            "name": "6_ensure_fresh_err_false",
            "payload": {
                "sampling_config": {"temperature": 0.2},
                "adapter": "anthropic",
                "record_rate_limit": {"ok": {"retry_after_secs": None}},
                "has_usable_accounts": True,
                "ensure_fresh": {"err": {"message": "cred stale – 更新失敗"}},
                "error": {"message": "⏳ too many requests"},
                "expected_prepare_sampler_for_turn_calls": 0,
            },
            "expected_result": False,
        },
        {
            "name": "7_all_pass_true_prepare_once",
            "payload": {
                "sampling_config": {"temperature": 0.2},
                "adapter": "anthropic",
                "record_rate_limit": {"ok": {"retry_after_secs": 0}},
                "has_usable_accounts": True,
                "ensure_fresh": {"ok": True},
                "error": {"message": "再試行してください 🔁"},
                "expected_prepare_sampler_for_turn_calls": 1,
            },
            "expected_result": True,
        },
    ]

    selected_path = None
    for path in candidate_paths:
        probe_payload = {
            "sampling_config": None,
            "adapter": "anthropic",
            "record_rate_limit": {"ok": {"retry_after_secs": None}},
            "has_usable_accounts": True,
            "ensure_fresh": {"ok": True},
            "error": {"message": "probe"},
            "expected_prepare_sampler_for_turn_calls": 0,
        }
        resp, body = _post_json(path, probe_payload)
        if resp.status_code != 404:
            selected_path = path
            break

    assert selected_path is not None, "No compatible harness endpoint found (all candidates returned 404)."

    for case in subcases:
        resp, body = _post_json(selected_path, case["payload"])
        assert resp.status_code == 200, f"{case['name']}: expected 200, got {resp.status_code}, body={body!r}"
        _assert_common_shape(body)
        assert body["result"] is case["expected_result"], (
            f"{case['name']}: expected result={case['expected_result']}, got {body['result']}, body={body!r}"
        )

        if "prepare_sampler_for_turn_calls" in body:
            assert body["prepare_sampler_for_turn_calls"] == case["payload"]["expected_prepare_sampler_for_turn_calls"], (
                f"{case['name']}: expected prepare_sampler_for_turn_calls="
                f"{case['payload']['expected_prepare_sampler_for_turn_calls']}, "
                f"got {body['prepare_sampler_for_turn_calls']}"
            )
        elif "calls" in body and isinstance(body["calls"], dict) and "prepare_sampler_for_turn" in body["calls"]:
            assert body["calls"]["prepare_sampler_for_turn"] == case["payload"]["expected_prepare_sampler_for_turn_calls"], (
                f"{case['name']}: expected calls.prepare_sampler_for_turn="
                f"{case['payload']['expected_prepare_sampler_for_turn_calls']}, "
                f"got {body['calls']['prepare_sampler_for_turn']}"
            )
        elif case["name"] == "7_all_pass_true_prepare_once":
            raise AssertionError(
                f"{case['name']}: response missing call-count evidence for prepare_sampler_for_turn. body={body!r}"
            )

test_bnd008_rotate_anthropic_after_rate_limit()