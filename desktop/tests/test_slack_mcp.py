import http.client
import json
import sys
import urllib.error
from pathlib import Path

import pytest


sys.path.insert(0, str(Path(__file__).parents[1] / "scripts"))

import slack_mcp


def _call(server, name, arguments):
    return server.jsonrpc(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
    )["result"]


def _authorize(server):
    server._authorized = True


def test_protocol_lists_exact_tools_without_loading_session():
    server = slack_mcp.SlackMCPServer()
    response = server.jsonrpc({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})

    assert [tool["name"] for tool in response["result"]["tools"]] == [
        "users_search",
        "channels_list",
        "conversations_add_message",
        "conversations_history",
        "conversations_replies",
        "reactions_add",
        "reactions_remove",
        "search_messages",
    ]
    assert server._credentials is None
    assert server.jsonrpc({"jsonrpc": "2.0", "method": "notifications/initialized"}) is None


def test_users_search_returns_only_exact_normalized_matches(monkeypatch):
    server = slack_mcp.SlackMCPServer()
    _authorize(server)
    monkeypatch.setattr(
        server,
        "_api",
        lambda method, values=None: {
            "ok": True,
            "members": [
                {
                    "id": "U1",
                    "name": "ishaan",
                    "profile": {"display_name": "Ishaan Mittal", "real_name": "Ishaan Mittal", "email": "ishaan@example.com"},
                },
                {"id": "U2", "name": "ishaani", "profile": {"display_name": "Ishaani Mittal"}},
            ],
        },
    )

    result = _call(server, "users_search", {"query": "Ishaan Mittal"})

    assert not result["isError"]
    assert json.loads(result["content"][0]["text"]) == [
        {"id": "U1", "name": "ishaan", "display_name": "Ishaan Mittal"}
    ]


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


def test_channels_list_uses_org_wide_counts_and_paces_name_lookups(monkeypatch):
    server = slack_mcp.SlackMCPServer()
    _authorize(server)
    clock = FakeClock()
    monkeypatch.setattr(slack_mcp, "time", clock)
    info = {
        "C1": {"id": "C1", "name": "general"},
        "CORG": {"id": "CORG", "name": "all-hands", "is_org_shared": True},
        "C2": {"id": "C2", "name": "old", "is_archived": True},
    }
    calls = []

    def api(method, values=None):
        calls.append((clock.now, method, values))
        if method == "client.counts":
            return {"channels": [{"id": "C1"}, {"id": "CORG"}, {"id": "C2"}], "ims": [{"id": "D1"}]}
        return {"channel": info[values["channel"]]}

    monkeypatch.setattr(server, "_api", api)

    result = _call(server, "channels_list", {})

    assert json.loads(result["content"][0]["text"]) == [{"id": "C1", "name": "general"}, {"id": "CORG", "name": "all-hands"}]
    assert calls[0] == (0.0, "client.counts", {"org_wide_aware": "true"})
    assert [(method, values["channel"]) for _, method, values in calls[1:]] == [
        ("conversations.info", "C1"),
        ("conversations.info", "CORG"),
        ("conversations.info", "C2"),
    ]
    assert [at for at, _, _ in calls] == pytest.approx([0.0, 1.1, 2.2, 3.3])


def test_rate_limit_backs_off_then_reports_rate_limited_past_deadline(monkeypatch):
    server = slack_mcp.SlackMCPServer()
    _authorize(server)
    server._credentials = ("xoxc-secret", "xoxd-secret")
    clock = FakeClock()
    monkeypatch.setattr(slack_mcp, "time", clock)

    class RateLimitedHTTP:
        def __init__(self):
            self.retry_after = ["2", "30"]
            self.calls = []

        def open(self, request, timeout):
            self.calls.append(clock.now)
            raise urllib.error.HTTPError(request.full_url, 429, "Too Many Requests", {"Retry-After": self.retry_after.pop(0)}, None)

    server._http = RateLimitedHTTP()

    result = _call(server, "channels_list", {})

    assert result["isError"]
    assert result["content"][0]["text"] == "slack_rate_limited"
    assert server._http.calls == [0.0, 2.0]


@pytest.mark.parametrize(
    "failure",
    [urllib.error.URLError("failed"), http.client.IncompleteRead(b"partial"), OSError("failed")],
)
def test_send_is_called_once_when_delivery_is_uncertain(failure):
    server = slack_mcp.SlackMCPServer()
    _authorize(server)
    server._credentials = ("xoxc-secret", "xoxd-secret")
    server._user_id = "UOWNER"

    class FailingHTTP:
        def __init__(self):
            self.calls = 0

        def open(self, request, timeout):
            self.calls += 1
            raise failure

    server._http = FailingHTTP()

    result = _call(server, "conversations_add_message", {"channel_id": "C123", "text": "hello"})
    output = json.dumps(result)

    assert result["isError"]
    assert result["content"][0]["text"] == "delivery_uncertain_message_may_have_been_sent"
    assert server._http.calls == 1
    assert "xoxc-secret" not in output
    assert "xoxd-secret" not in output


@pytest.mark.parametrize(
    "payload",
    [["invalid"], {"ok": False, "error": "internal_error"}, {"ok": False, "error": "fatal_error"}],
)
def test_send_ambiguous_response_is_not_retried_or_exposed(monkeypatch, payload):
    server = slack_mcp.SlackMCPServer()
    _authorize(server)
    server._credentials = ("xoxc-secret", "xoxd-secret")
    server._user_id = "UOWNER"

    class HTTP:
        def __init__(self):
            self.calls = 0

        def open(self, request, timeout):
            self.calls += 1
            return self

        def __enter__(self):
            return self

        def __exit__(self, kind, value, traceback):
            return False

    server._http = HTTP()
    monkeypatch.setattr(slack_mcp.json, "load", lambda response: payload)

    result = _call(server, "conversations_add_message", {"channel_id": "C123", "text": "hello"})
    output = json.dumps(result)

    assert result["isError"]
    assert result["content"][0]["text"] == "delivery_uncertain_message_may_have_been_sent"
    assert server._http.calls == 1
    assert "xoxc-secret" not in output
    assert "xoxd-secret" not in output


def test_send_verifies_success_and_ignores_permalink_failure(monkeypatch):
    server = slack_mcp.SlackMCPServer()
    _authorize(server)
    server._user_id = "UOWNER"
    calls = []

    def api(method, values=None):
        calls.append((method, values))
        if method == "users.info":
            return {"ok": True, "user": {"id": "URECIPIENT"}}
        if method == "conversations.open":
            assert values == {"users": "URECIPIENT", "return_im": "true"}
            return {"ok": True, "channel": {"id": "D123", "user": "URECIPIENT"}}
        if method == "chat.postMessage":
            return {"ok": True, "channel": "D123", "ts": "123.456", "message": {"user": "UOWNER"}}
        if method == "chat.getPermalink":
            raise slack_mcp.SlackMCPError("slack_network_failed")
        raise AssertionError(method)

    monkeypatch.setattr(server, "_api", api)

    result = _call(server, "conversations_add_message", {"channel_id": "URECIPIENT", "text": "hello"})
    output = json.loads(result["content"][0]["text"])

    assert not result["isError"]
    assert output == {"sent": True, "verified": True, "channel_id": "D123", "ts": "123.456"}
    assert [method for method, _ in calls] == [
        "users.info",
        "conversations.open",
        "chat.postMessage",
        "chat.getPermalink",
    ]


def test_connection_gate_rejects_missing_and_wrong_key_before_recovery(monkeypatch, tmp_path):
    connection_path = tmp_path / "dev.ygpt.loka.pro/slack-connection.key"
    connection_path.parent.mkdir(mode=0o700)
    connection_path.write_text("local-key")
    connection_path.chmod(0o600)
    monkeypatch.setattr(slack_mcp, "CONNECTION_PATH", connection_path)
    monkeypatch.delenv("WORLD_SLACK_CONNECTION_KEY", raising=False)

    def recover_session(team_id, user_id):
        raise AssertionError

    monkeypatch.setattr(slack_mcp, "recover_session", recover_session)

    missing = _call(slack_mcp.SlackMCPServer(), "channels_list", {})
    monkeypatch.setenv("WORLD_SLACK_CONNECTION_KEY", "wrong-key")
    wrong = _call(slack_mcp.SlackMCPServer(), "channels_list", {})

    assert missing["isError"]
    assert wrong["isError"]
    assert missing["content"][0]["text"] == "slack_connection_not_authorized"
    assert wrong["content"][0]["text"] == "slack_connection_not_authorized"


def test_connection_gate_allows_matching_key(monkeypatch, tmp_path):
    connection_path = tmp_path / "dev.ygpt.loka.pro/slack-connection.key"
    connection_path.parent.mkdir(mode=0o700)
    connection_path.write_text("matching-key")
    connection_path.chmod(0o600)
    monkeypatch.setattr(slack_mcp, "CONNECTION_PATH", connection_path)
    monkeypatch.setenv("WORLD_SLACK_CONNECTION_KEY", "matching-key")
    server = slack_mcp.SlackMCPServer()
    monkeypatch.setattr(server, "channels_list", lambda arguments: [{"id": "C1", "name": "general"}])

    result = _call(server, "channels_list", {})

    assert not result["isError"]
    assert server._authorized
    assert server._connection_key is None


def test_auth_failure_recovers_a_fresh_session_and_retries(monkeypatch):
    monkeypatch.setenv("WORLD_SLACK_TEAM_ID", "T1")
    monkeypatch.setenv("WORLD_SLACK_USER_ID", "U1")
    recovered = []

    def recover_session(team_id, user_id):
        recovered.append(team_id)
        return (f"xoxc-{len(recovered)}", "cookie")

    monkeypatch.setattr(slack_mcp, "recover_session", recover_session)
    server = slack_mcp.SlackMCPServer()
    seen = []

    def request(method, values=None):
        token, _ = server._session()
        seen.append(token)
        if token == "xoxc-1":
            raise slack_mcp.SlackMCPError("invalid_auth")
        return {"ok": True, "channels": []}

    monkeypatch.setattr(server, "_request", request)

    assert server._api("conversations.list") == {"ok": True, "channels": []}
    assert seen == ["xoxc-1", "xoxc-2"]


def test_discovers_the_only_signed_in_workspace_without_identity_env(monkeypatch):
    monkeypatch.delenv("WORLD_SLACK_TEAM_ID", raising=False)
    monkeypatch.delenv("WORLD_SLACK_USER_ID", raising=False)
    monkeypatch.setattr(
        slack_mcp,
        "discover_sessions",
        lambda: [{"token": "xoxc-a", "cookie": "d", "team_id": "T1", "user_id": "U9"}],
    )
    server = slack_mcp.SlackMCPServer()

    assert server._session() == ("xoxc-a", "d")
    assert server._user_id == "U9"
