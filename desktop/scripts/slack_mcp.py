import http.client
import hmac
import json
import logging
import os
import re
import stat
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from slack_session import SlackSessionError, _opener, discover_sessions, recover_session


LOGGER = logging.getLogger("slack-mcp")
CONNECTION_PATH = Path.home() / "Library/Application Support/dev.ygpt.loka.pro/slack-connection.key"
METHODS = {
    "users.list",
    "users.info",
    "client.counts",
    "conversations.info",
    "conversations.open",
    "chat.postMessage",
    "chat.getPermalink",
    "conversations.history",
    "conversations.replies",
    "search.messages",
    "reactions.add",
    "reactions.remove",
}
AUTH_ERRORS = {"invalid_auth", "not_authed", "token_revoked", "token_expired", "account_inactive"}
PACE_SECS = 1.1
CHANNELS_DEADLINE_SECS = 25


class SlackMCPError(Exception):
    pass


class SlackRateLimited(SlackMCPError):
    def __init__(self, retry_after):
        super().__init__("slack_rate_limited")
        self.retry_after = retry_after


def _normalized(value):
    return "".join(character for character in value.casefold() if character.isalnum())


def _slack_error(result):
    error = result.get("error")
    return error if isinstance(error, str) and re.fullmatch(r"[a-z0-9_]+", error) else "slack_api_failed"


def _message(message):
    return {key: message[key] for key in ("ts", "user", "text", "thread_ts", "reply_count") if key in message}


class SlackMCPServer:
    def __init__(self):
        self._connection_key = os.environ.pop("WORLD_SLACK_CONNECTION_KEY", None)
        self._authorized = False
        self._credentials = None
        self._user_id = None
        self._directory = None
        self._channels = {}
        self._next_call = 0.0
        self._http = _opener()

    def _authorize(self):
        if self._authorized:
            return
        try:
            parent = CONNECTION_PATH.parent.stat()
            connection = CONNECTION_PATH.stat()
            local_key = CONNECTION_PATH.read_text().strip()
        except OSError:
            raise SlackMCPError("slack_connection_not_authorized") from None
        if (
            not self._connection_key
            or parent.st_uid != os.getuid()
            or connection.st_uid != os.getuid()
            or stat.S_IMODE(parent.st_mode) != 0o700
            or stat.S_IMODE(connection.st_mode) != 0o600
            or not hmac.compare_digest(self._connection_key, local_key)
        ):
            raise SlackMCPError("slack_connection_not_authorized")
        self._connection_key = None
        self._authorized = True

    def _session(self):
        if self._credentials:
            return self._credentials
        team_id = os.environ.get("WORLD_SLACK_TEAM_ID", "")
        user_id = os.environ.get("WORLD_SLACK_USER_ID", "")
        if not team_id and not user_id:
            return self._discovered()
        if not re.fullmatch(r"T[A-Z0-9]+", team_id) or not re.fullmatch(r"[UW][A-Z0-9]+", user_id):
            raise SlackMCPError("slack_identity_config_invalid")
        try:
            self._credentials = recover_session(team_id, user_id)
        except SlackSessionError as error:
            raise SlackMCPError(str(error)) from None
        self._user_id = user_id
        return self._credentials

    def _discovered(self):
        try:
            sessions = discover_sessions()
        except SlackSessionError as error:
            raise SlackMCPError(str(error)) from None
        if len(sessions) != 1:
            raise SlackMCPError("slack_multiple_workspaces_set_WORLD_SLACK_TEAM_ID_and_WORLD_SLACK_USER_ID")
        session = sessions[0]
        self._credentials = (session["token"], session["cookie"])
        self._user_id = session["user_id"]
        return self._credentials

    def _forget_session(self):
        self._credentials = None
        self._directory = None

    def _api(self, method, values=None):
        try:
            return self._request(method, values)
        except SlackMCPError as error:
            if str(error) not in AUTH_ERRORS:
                raise
        self._forget_session()
        return self._request(method, values)

    def _request(self, method, values=None):
        if method not in METHODS:
            raise SlackMCPError("slack_method_rejected")
        token, cookie = self._session()
        request = urllib.request.Request(
            f"https://slack.com/api/{method}",
            data=urllib.parse.urlencode(values or {}).encode(),
            headers={
                "Authorization": f"Bearer {token}",
                "Cookie": f"d={cookie}",
                "Content-Type": "application/x-www-form-urlencoded",
            },
            method="POST",
        )
        try:
            with self._http.open(request, timeout=20) as response:
                result = json.load(response)
        except urllib.error.HTTPError as error:
            if method == "chat.postMessage":
                raise SlackMCPError("delivery_uncertain_message_may_have_been_sent") from None
            if 300 <= error.code < 400:
                raise SlackMCPError("redirect_rejected") from None
            if error.code == 429:
                raise SlackRateLimited(int(error.headers.get("Retry-After") or 30)) from None
            raise SlackMCPError("slack_http_failed") from None
        except (urllib.error.URLError, TimeoutError, http.client.HTTPException, OSError):
            error = (
                "delivery_uncertain_message_may_have_been_sent"
                if method == "chat.postMessage"
                else "slack_network_failed"
            )
            raise SlackMCPError(error) from None
        except (json.JSONDecodeError, UnicodeDecodeError):
            error = (
                "delivery_uncertain_message_may_have_been_sent"
                if method == "chat.postMessage"
                else "slack_response_invalid"
            )
            raise SlackMCPError(error) from None
        if not isinstance(result, dict):
            error = (
                "delivery_uncertain_message_may_have_been_sent"
                if method == "chat.postMessage"
                else "slack_response_invalid"
            )
            raise SlackMCPError(error)
        if not result.get("ok"):
            error = _slack_error(result)
            if method == "chat.postMessage" and error in {"internal_error", "fatal_error"}:
                error = "delivery_uncertain_message_may_have_been_sent"
            raise SlackMCPError(error)
        return result

    def _users(self):
        if self._directory is not None:
            return self._directory
        users = []
        cursor = ""
        while True:
            values = {"limit": 200}
            if cursor:
                values["cursor"] = cursor
            result = self._api("users.list", values)
            for member in result.get("members") or []:
                member_id = member.get("id")
                if member.get("deleted") or member.get("is_bot") or not isinstance(member_id, str):
                    continue
                profile = member.get("profile") or {}
                users.append(
                    {
                        "id": member_id,
                        "name": member.get("name") or "",
                        "display_name": profile.get("display_name") or profile.get("real_name") or "",
                        "search": tuple(
                            value
                            for value in (
                                member.get("name"),
                                profile.get("display_name"),
                                profile.get("real_name"),
                                profile.get("email"),
                            )
                            if isinstance(value, str)
                        ),
                    }
                )
            cursor = (result.get("response_metadata") or {}).get("next_cursor") or ""
            if not cursor:
                break
        self._directory = users
        return users

    def users_search(self, arguments):
        query = arguments.get("query")
        if not isinstance(query, str) or not query.strip():
            raise SlackMCPError("query_required")
        query = _normalized(query)
        matches = []
        for user in self._users():
            if query and any(_normalized(value) == query for value in user["search"]):
                matches.append({key: user[key] for key in ("id", "name", "display_name")})
        return matches

    def _paced(self, method, values, deadline):
        while True:
            time.sleep(max(0.0, self._next_call - time.monotonic()))
            self._next_call = time.monotonic() + PACE_SECS
            try:
                return self._api(method, values)
            except SlackRateLimited as limited:
                if time.monotonic() + limited.retry_after > deadline:
                    raise
                self._next_call = time.monotonic() + max(limited.retry_after, PACE_SECS)

    def channels_list(self, arguments):
        deadline = time.monotonic() + CHANNELS_DEADLINE_SECS
        counts = self._paced("client.counts", {"org_wide_aware": "true"}, deadline)
        channels = []
        for entry in counts.get("channels") or []:
            if not isinstance(entry, dict) or not isinstance(entry.get("id"), str):
                continue
            channel_id = entry["id"]
            if channel_id not in self._channels and self._next_call < deadline:
                try:
                    self._channels[channel_id] = self._paced("conversations.info", {"channel": channel_id}, deadline).get("channel") or {}
                except SlackRateLimited:
                    deadline = time.monotonic()
                except SlackMCPError:
                    pass
            channel = self._channels.get(channel_id, {})
            if not channel.get("is_archived"):
                channels.append({"id": channel_id, "name": channel.get("name") or channel_id})
        return channels

    def conversations_add_message(self, arguments):
        channel_id = arguments.get("channel_id")
        text = arguments.get("text")
        thread_ts = arguments.get("thread_ts")
        if not isinstance(channel_id, str) or not re.fullmatch(r"[CDGUW][A-Z0-9]+", channel_id):
            raise SlackMCPError("channel_id_invalid")
        if not isinstance(text, str) or not text.strip() or len(text) > 40000:
            raise SlackMCPError("text_invalid")
        if thread_ts is not None and (not isinstance(thread_ts, str) or not re.fullmatch(r"\d+\.\d+", thread_ts)):
            raise SlackMCPError("thread_ts_invalid")
        if channel_id[0] in "UW":
            user = self._api("users.info", {"user": channel_id}).get("user") or {}
            if user.get("id") != channel_id or user.get("deleted") or user.get("is_bot"):
                raise SlackMCPError("recipient_invalid")
            opened = self._api("conversations.open", {"users": channel_id, "return_im": "true"}).get("channel") or {}
            if not re.fullmatch(r"D[A-Z0-9]+", opened.get("id", "")) or opened.get("user") != channel_id:
                raise SlackMCPError("dm_verification_failed")
            channel_id = opened["id"]
        values = {"channel": channel_id, "text": text}
        if thread_ts:
            values["thread_ts"] = thread_ts
        posted = self._api("chat.postMessage", values)
        message = posted.get("message") or {}
        timestamp = posted.get("ts")
        if posted.get("channel") != channel_id or message.get("user") != self._user_id or not isinstance(timestamp, str):
            raise SlackMCPError("delivery_uncertain_message_may_have_been_sent")
        output = {"sent": True, "verified": True, "channel_id": channel_id, "ts": timestamp}
        try:
            permalink = self._api("chat.getPermalink", {"channel": channel_id, "message_ts": timestamp}).get("permalink")
        except SlackMCPError:
            permalink = None
        if isinstance(permalink, str):
            parsed = urllib.parse.urlparse(permalink)
            if parsed.scheme == "https" and parsed.hostname and (
                parsed.hostname == "slack.com" or parsed.hostname.endswith(".slack.com")
            ):
                output["permalink"] = permalink
        return output

    def conversations_history(self, arguments):
        channel_id = arguments.get("channel_id")
        if not isinstance(channel_id, str) or not re.fullmatch(r"[CDG][A-Z0-9]+", channel_id):
            raise SlackMCPError("channel_id_invalid")
        result = self._api("conversations.history", {"channel": channel_id, "limit": 50})
        return [_message(message) for message in result.get("messages") or []]

    def conversations_replies(self, arguments):
        channel_id = arguments.get("channel_id")
        ts = arguments.get("ts")
        if not isinstance(channel_id, str) or not re.fullmatch(r"[CDG][A-Z0-9]+", channel_id):
            raise SlackMCPError("channel_id_invalid")
        if not isinstance(ts, str) or not re.fullmatch(r"\d+\.\d+", ts):
            raise SlackMCPError("ts_invalid")
        result = self._api("conversations.replies", {"channel": channel_id, "ts": ts, "limit": 200})
        return [_message(message) for message in result.get("messages") or []]

    def _reaction(self, method, arguments, harmless):
        channel_id = arguments.get("channel_id")
        ts = arguments.get("ts")
        name = arguments.get("name")
        if not isinstance(channel_id, str) or not re.fullmatch(r"[CDG][A-Z0-9]+", channel_id):
            raise SlackMCPError("channel_id_invalid")
        if not isinstance(ts, str) or not re.fullmatch(r"\d+\.\d+", ts):
            raise SlackMCPError("ts_invalid")
        name = name.strip().strip(":") if isinstance(name, str) else ""
        if not re.fullmatch(r"[a-z0-9_+'-]+(::skin-tone-[2-6])?", name):
            raise SlackMCPError("name_invalid")
        output = {"ok": True, "channel_id": channel_id, "ts": ts, "name": name}
        try:
            self._api(method, {"channel": channel_id, "timestamp": ts, "name": name})
        except SlackMCPError as error:
            if str(error) != harmless:
                raise
            output["note"] = harmless
        return output

    def reactions_add(self, arguments):
        return self._reaction("reactions.add", arguments, "already_reacted")

    def reactions_remove(self, arguments):
        return self._reaction("reactions.remove", arguments, "no_reaction")

    def search_messages(self, arguments):
        query = arguments.get("query")
        if not isinstance(query, str) or not query.strip():
            raise SlackMCPError("query_required")
        result = self._api("search.messages", {"query": query, "count": 20, "sort": "timestamp"})
        return [
            {
                "channel_id": (match.get("channel") or {}).get("id"),
                "permalink": match.get("permalink"),
                **_message(match),
            }
            for match in (result.get("messages") or {}).get("matches") or []
        ]

    def tools(self):
        return [
            {
                "name": "users_search",
                "description": "Find an active Slack person by exact name, handle, or email.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"],
                },
            },
            {
                "name": "channels_list",
                "description": "List joined, active public and private Slack channels.",
                "inputSchema": {"type": "object", "properties": {}},
            },
            {
                "name": "conversations_add_message",
                "description": "Send a Slack message to a person or conversation ID. Set thread_ts to the parent message ts to reply in its thread.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "channel_id": {"type": "string"},
                        "text": {"type": "string", "maxLength": 40000},
                        "thread_ts": {"type": "string"},
                    },
                    "required": ["channel_id", "text"],
                },
            },
            {
                "name": "conversations_history",
                "description": "Read recent messages in a Slack channel or DM by conversation ID.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"channel_id": {"type": "string"}},
                    "required": ["channel_id"],
                },
            },
            {
                "name": "conversations_replies",
                "description": "Read a Slack thread by conversation ID and parent message ts.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "channel_id": {"type": "string"},
                        "ts": {"type": "string"},
                    },
                    "required": ["channel_id", "ts"],
                },
            },
            {
                "name": "reactions_add",
                "description": "Add an emoji reaction to a Slack message by conversation ID and message ts. name is the emoji name, like ticket.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "channel_id": {"type": "string"},
                        "ts": {"type": "string"},
                        "name": {"type": "string"},
                    },
                    "required": ["channel_id", "ts", "name"],
                },
            },
            {
                "name": "reactions_remove",
                "description": "Remove your emoji reaction from a Slack message by conversation ID and message ts.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "channel_id": {"type": "string"},
                        "ts": {"type": "string"},
                        "name": {"type": "string"},
                    },
                    "required": ["channel_id", "ts", "name"],
                },
            },
            {
                "name": "search_messages",
                "description": "Search Slack messages with Slack search syntax.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"],
                },
            },
        ]

    def call(self, name, arguments):
        if not isinstance(arguments, dict):
            raise SlackMCPError("arguments_invalid")
        if name == "users_search":
            return self.users_search(arguments)
        if name == "channels_list":
            return self.channels_list(arguments)
        if name == "conversations_add_message":
            return self.conversations_add_message(arguments)
        if name == "conversations_history":
            return self.conversations_history(arguments)
        if name == "conversations_replies":
            return self.conversations_replies(arguments)
        if name == "search_messages":
            return self.search_messages(arguments)
        if name == "reactions_add":
            return self.reactions_add(arguments)
        if name == "reactions_remove":
            return self.reactions_remove(arguments)
        raise SlackMCPError("tool_not_found")

    def jsonrpc(self, message):
        if not isinstance(message, dict) or "id" not in message:
            return None
        request_id = message["id"]
        method = message.get("method")
        params = message.get("params") if isinstance(message.get("params"), dict) else {}
        if method == "initialize":
            result = {
                "protocolVersion": params.get("protocolVersion", "2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "world-slack", "version": "1.0.0"},
            }
            return {"jsonrpc": "2.0", "id": request_id, "result": result}
        if method == "ping":
            return {"jsonrpc": "2.0", "id": request_id, "result": {}}
        if method == "tools/list":
            return {"jsonrpc": "2.0", "id": request_id, "result": {"tools": self.tools()}}
        if method == "tools/call":
            try:
                self._authorize()
                output = self.call(params.get("name"), params.get("arguments") or {})
                text = json.dumps(output, separators=(",", ":"))
                is_error = False
            except SlackMCPError as error:
                text = str(error)
                is_error = True
            result = {"content": [{"type": "text", "text": text}], "isError": is_error}
            return {"jsonrpc": "2.0", "id": request_id, "result": result}
        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32601, "message": "method not found"},
        }


def serve():
    server = SlackMCPServer()
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        response = server.jsonrpc(message)
        if response is not None:
            sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
            sys.stdout.flush()


def main():
    logging.basicConfig(level=logging.ERROR, format="%(message)s")
    try:
        serve()
    except (OSError, SlackMCPError) as error:
        LOGGER.error(str(error))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
