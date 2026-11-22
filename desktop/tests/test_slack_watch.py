import json
import os
import sys
from pathlib import Path


sys.path.insert(0, str(Path(__file__).parents[1] / "scripts"))

import slack_session
import slack_watch


SESSION = {
    "token": "xoxc-test",
    "cookie": "xoxd-test",
    "team_id": "T1",
    "user_id": "UME",
    "team": "Acme",
    "url": "https://acme.slack.com/",
}


class FakeWorkspace(slack_watch.Workspace):
    def __init__(self, responses):
        super().__init__(SESSION, http=None)
        self.responses = responses
        self.calls = []

    def api(self, method, values=None):
        self.calls.append((method, values or {}))
        response = self.responses[method]
        return response(values or {}) if callable(response) else response


def _state():
    return {"started": "100.000000", "cursors": {}}


def _users(values):
    return {"user": {"profile": {"display_name": {"UANA": "Ana", "UBO": "Bo"}.get(values["user"], "")}}}


def test_new_direct_message_becomes_a_dm_notification():
    workspace = FakeWorkspace(
        {
            "client.counts": {"ims": [{"id": "D1", "latest": "105.000001", "last_read": "90.000000"}], "channels": [], "threads": {}},
            "conversations.history": {
                "messages": [
                    {"ts": "105.000001", "user": "UANA", "text": "hey <@UME> got a sec?"},
                    {"ts": "104.000000", "user": "UME", "text": "my own message"},
                    {"ts": "99.000000", "user": "UANA", "text": "before the watcher started"},
                ]
            },
            "conversations.info": {"channel": {"is_im": True, "user": "UANA"}},
            "users.info": _users,
            "users.prefs.get": {"prefs": {}},
        }
    )
    state = _state()
    found = workspace.poll(state)
    assert [(n["kind"], n["ts"], n["author"], n["channel_name"]) for n in found] == [("dm", "105.000001", "Ana", "Ana")]
    assert found[0]["text"] == "hey @UME got a sec?"
    assert found[0]["permalink"] == "https://acme.slack.com/archives/D1/p105000001"
    assert state["cursors"]["D1"] == "105.000001"
    assert workspace.poll(state) == []


def test_channels_only_fetch_history_when_slack_counts_a_mention():
    workspace = FakeWorkspace(
        {
            "client.counts": {
                "ims": [],
                "channels": [
                    {"id": "CQUIET", "latest": "110.000000", "last_read": "100.000000", "mention_count": 0},
                    {"id": "CLOUD", "latest": "111.000000", "last_read": "100.000000", "mention_count": 1},
                ],
                "threads": {},
            },
            "conversations.history": {
                "messages": [
                    {"ts": "111.000000", "user": "UBO", "text": "<@UME> can you review?"},
                    {"ts": "110.500000", "user": "UBO", "text": "unrelated chatter"},
                ]
            },
            "conversations.info": {"channel": {"name": "eng"}},
            "users.info": _users,
            "users.prefs.get": {"prefs": {"highlight_words": "deploy"}},
        }
    )
    state = _state()
    found = workspace.poll(state)
    assert [(n["kind"], n["channel_id"], n["channel_name"], n["text"]) for n in found] == [("mention", "CLOUD", "eng", "@UME can you review?")]
    histories = [values["channel"] for method, values in workspace.calls if method == "conversations.history"]
    assert histories == ["CLOUD"]
    assert state["cursors"] == {"CQUIET": "110.000000", "CLOUD": "111.000000"}


def test_highlight_words_and_read_messages():
    workspace = FakeWorkspace(
        {
            "client.counts": {"ims": [], "channels": [{"id": "C1", "latest": "120.000000", "last_read": "115.000000", "mention_count": 1}], "threads": {}},
            "conversations.history": {
                "messages": [
                    {"ts": "112.000000", "user": "UBO", "text": "Deploy is broken"},
                    {"ts": "120.000000", "user": "UBO", "text": "the deploy is broken again"},
                    {"ts": "119.000000", "user": "UBO", "text": "redeployment later"},
                ]
            },
            "conversations.info": {"channel": {"name": "ops"}},
            "users.info": _users,
            "users.prefs.get": {"prefs": {"highlight_words": "deploy, outage"}},
        }
    )
    found = workspace.poll(_state())
    assert [(n["kind"], n["ts"]) for n in found] == [("highlight", "120.000000")]


def test_thread_replies_from_others_are_reported_once():
    workspace = FakeWorkspace(
        {
            "client.counts": {"ims": [], "channels": [], "threads": {"has_unreads": True}},
            "subscriptions.thread.getView": {
                "threads": [
                    {
                        "root_msg": {"channel": "C9", "ts": "101.000000"},
                        "unread_replies": [
                            {"ts": "130.000000", "user": "UBO", "text": "replied"},
                            {"ts": "131.000000", "user": "UME", "text": "me too"},
                        ],
                    }
                ]
            },
            "conversations.info": {"channel": {"name": "design"}},
            "users.info": _users,
            "users.prefs.get": {"prefs": {}},
        }
    )
    state = _state()
    found = workspace.poll(state)
    assert [(n["kind"], n["thread_ts"], n["channel_id"]) for n in found] == [("thread", "101.000000", "C9")]
    assert "thread_ts=101.000000" in found[0]["permalink"]
    assert workspace.poll(state) == []


def test_watched_channels_report_every_new_message():
    workspace = FakeWorkspace(
        {
            "client.counts": {
                "ims": [],
                "channels": [
                    {"id": "CWATCH", "latest": "112.000000", "last_read": "112.000000", "mention_count": 0},
                    {"id": "COTHER", "latest": "112.000000", "last_read": "100.000000", "mention_count": 0},
                ],
                "threads": {},
            },
            "conversations.history": {
                "messages": [
                    {"ts": "111.000000", "user": "UBO", "text": "<@UME> look"},
                    {"ts": "110.000000", "user": "UBO", "text": "deploy started"},
                    {"ts": "110.500000", "user": "UME", "text": "my own"},
                ]
            },
            "conversations.info": {"channel": {"name": "ops"}},
            "users.info": _users,
            "users.prefs.get": {"prefs": {}},
        }
    )
    watches = slack_watch.parse_watches({"type": "watches", "channels": [{"channel_id": "CWATCH", "team_id": None}, {"channel_id": "CELSE", "team_id": "T9"}]})
    state = _state()
    found = workspace.poll(state, watches)
    assert [(n["kind"], n["channel_id"], n["ts"]) for n in found] == [("watched", "CWATCH", "110.000000"), ("mention", "CWATCH", "111.000000")]
    histories = [values["channel"] for method, values in workspace.calls if method == "conversations.history"]
    assert histories == ["CWATCH"]
    assert workspace.poll(state, watches) == []


def test_watched_threads_report_new_replies_within_budget():
    replies = {
        "messages": [
            {"ts": "100.000000", "user": "UBO", "text": "root"},
            {"ts": "140.000000", "user": "UBO", "text": "new reply", "thread_ts": "100.000000"},
            {"ts": "141.000000", "user": "UME", "text": "mine", "thread_ts": "100.000000"},
        ]
    }
    workspace = FakeWorkspace(
        {
            "client.counts": {"ims": [], "channels": [{"id": "C7", "latest": "90.000000"}], "threads": {}},
            "conversations.replies": replies,
            "conversations.info": {"channel": {"name": "eng"}},
            "users.info": _users,
            "users.prefs.get": {"prefs": {}},
        }
    )
    watches = slack_watch.parse_watches(
        {
            "threads": [
                {"channel_id": "C7", "thread_ts": "100.000000", "since": "130.000000"},
                {"channel_id": "CNOTMINE", "thread_ts": "100.000000", "since": "130.000000"},
            ]
        }
    )
    state = _state()
    found = workspace.poll(state, watches)
    assert [(n["kind"], n["channel_id"], n["ts"], n["thread_ts"]) for n in found] == [("watched", "C7", "140.000000", "100.000000")]
    assert "thread_ts=100.000000" in found[0]["permalink"]
    called = [values for method, values in workspace.calls if method == "conversations.replies"]
    assert called == [{"channel": "C7", "ts": "100.000000", "oldest": "130.000000", "limit": 50, "inclusive": "false"}]
    assert state["thread_cursors"]["C7|100.000000"] == "141.000000"
    assert workspace.poll(state, watches) == []
    assert len([m for m, _ in workspace.calls if m == "conversations.replies"]) == 1


def test_budget_limits_calls_per_minute():
    now = [0.0]
    budget = slack_watch.Budget(per_minute=2, clock=lambda: now[0])
    assert budget.take("conversations.replies")
    assert budget.take("conversations.replies")
    assert not budget.take("conversations.replies")
    assert budget.take("conversations.history")
    now[0] = 61.0
    assert budget.take("conversations.replies")


def test_watch_list_arrives_on_stdin():
    watcher = slack_watch.Watcher(4)
    watcher.listen(['not json\n', json.dumps({"type": "watches", "channels": [{"channel_id": "C1"}], "threads": [{"channel_id": "C2", "thread_ts": "1.000001"}]}) + "\n"])
    assert watcher.watches["channels"] == {"C1": None}
    assert watcher.watches["threads"] == {("C2", "1.000001"): (None, None)}


def test_workspace_status_includes_identity_for_retained_workspaces():
    workspace = FakeWorkspace({})
    status = slack_watch._status("slack_network_failed", [workspace])
    assert status["workspaces"] == [{"team_id": "T1", "user_id": "UME", "team": "Acme"}]
    assert status["error"] == "slack_network_failed"


def test_readable_formats_slack_markup():
    workspace = FakeWorkspace({"users.info": _users, "conversations.info": {"channel": {"name": "eng"}}})
    assert workspace.readable("hi <@UBO> see <#C1> and <https://x.dev|docs> <!here> &amp;") == "hi @Bo see #eng and docs @here &"


def test_bot_messages_and_system_subtypes():
    workspace = FakeWorkspace({})
    assert workspace.from_someone_else({"ts": "1.0", "bot_id": "B1", "subtype": "bot_message"})
    assert not workspace.from_someone_else({"ts": "1.0", "user": "UBO", "subtype": "channel_join"})
    assert not workspace.from_someone_else({"ts": "1.0", "user": "UME"})


def test_connection_key_must_match_private_file(tmp_path):
    directory = tmp_path / "world"
    directory.mkdir(mode=0o700)
    os.chmod(directory, 0o700)
    key_path = directory / "slack-connection.key"
    key_path.write_text("secret-key\n")
    os.chmod(key_path, 0o600)
    assert slack_session.verify_connection_key("secret-key", key_path)
    assert not slack_session.verify_connection_key("other", key_path)
    assert not slack_session.verify_connection_key(None, key_path)
    os.chmod(key_path, 0o644)
    assert not slack_session.verify_connection_key("secret-key", key_path)


def test_connection_key_is_found_in_the_loka_app_folder(monkeypatch, tmp_path):
    support = tmp_path / "Library/Application Support"

    def private_key(directory, key):
        directory.mkdir(parents=True, mode=0o700, exist_ok=True)
        os.chmod(directory, 0o700)
        path = directory / "slack-connection.key"
        path.write_text(key)
        os.chmod(path, 0o600)

    private_key(support / "dev.ygpt.loka", "free-key")
    private_key(tmp_path / "elsewhere", "planted-key")
    monkeypatch.setattr(slack_session.Path, "home", classmethod(lambda cls: tmp_path))
    assert slack_session.verify_connection_key("free-key")
    assert not slack_session.verify_connection_key("planted-key")
    assert not slack_session.verify_connection_key(None)


def test_discover_sessions_keeps_one_session_per_workspace(monkeypatch, tmp_path):
    monkeypatch.setattr(slack_session, "_find_tokens", lambda root: {b"xoxc-1111111111-aaaaaaaaaaaaaaaaaaaa", b"xoxc-2222222222-bbbbbbbbbbbbbbbbbbbb", b"xoxc-3333333333-cccccccccccccccccccc"})
    monkeypatch.setattr(slack_session, "_read_cookie", lambda root: (".slack.com", "xoxd-cookie", b"", 24))
    monkeypatch.setattr(slack_session, "_opener", object)
    identities = {
        "xoxc-1111111111-aaaaaaaaaaaaaaaaaaaa": {"ok": True, "team_id": "T1", "user_id": "U1", "team": "Acme", "url": "https://acme.slack.com/"},
        "xoxc-2222222222-bbbbbbbbbbbbbbbbbbbb": {"ok": False, "error": "invalid_auth"},
        "xoxc-3333333333-cccccccccccccccccccc": {"ok": True, "team_id": "T2", "user_id": "U2", "team": "Other", "url": "https://other.slack.com/"},
    }
    monkeypatch.setattr(slack_session, "_auth_test", lambda opener, token, cookie: identities[token])
    sessions = slack_session.discover_sessions(tmp_path)
    assert sorted((s["team_id"], s["team"]) for s in sessions) == [("T1", "Acme"), ("T2", "Other")]


def test_main_refuses_without_connection_key(monkeypatch, capsys, tmp_path):
    monkeypatch.delenv("WORLD_SLACK_CONNECTION_KEY", raising=False)
    monkeypatch.setattr(slack_watch, "CONNECTION_PATH", tmp_path / "missing.key")
    assert slack_watch.main() == 2
    assert json.loads(capsys.readouterr().out)["error"] == "slack_connection_not_authorized"
