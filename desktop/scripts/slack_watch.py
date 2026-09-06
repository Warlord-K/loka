import http.client
import json
import logging
import os
import re
import sys
import threading
import time
from collections import deque
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from slack_session import SlackSessionError, _opener, discover_sessions, verify_connection_key


LOGGER = logging.getLogger("slack-watch")
CONNECTION_PATH = Path.home() / "Library/Application Support/dev.ygpt.loka.pro/slack-connection.key"
METHODS = {
    "client.counts",
    "conversations.history",
    "conversations.info",
    "conversations.replies",
    "users.info",
    "users.prefs.get",
    "subscriptions.thread.getView",
}
MESSAGE_SUBTYPES = {None, "bot_message", "thread_broadcast", "file_share", "me_message"}
BROADCASTS = ("<!here", "<!channel", "<!everyone", "<!subteam^")
MAX_TEXT = 2000
PREFS_TTL = 600
WATCH_CALLS_PER_MINUTE = 30
THREAD_EVERY = 20
THREAD_CATCH_UP = 3600


class SlackWatchError(Exception):
    pass


class RateLimited(SlackWatchError):
    def __init__(self, retry_after):
        super().__init__("rate_limited")
        self.retry_after = retry_after


def emit(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def ts_value(ts):
    try:
        return float(ts)
    except (TypeError, ValueError):
        return 0.0


def newer(left, right):
    return ts_value(left) > ts_value(right)


def now_ts():
    return f"{time.time():.6f}"


class Budget:
    def __init__(self, per_minute=WATCH_CALLS_PER_MINUTE, clock=time.monotonic):
        self.per_minute = per_minute
        self.clock = clock
        self.calls = {}

    def take(self, method):
        now = self.clock()
        calls = self.calls.setdefault(method, deque())
        while calls and now - calls[0] >= 60:
            calls.popleft()
        if len(calls) >= self.per_minute:
            return False
        calls.append(now)
        return True


def parse_watches(message):
    channels = {}
    threads = {}
    for entry in message.get("channels") or []:
        if isinstance(entry, dict) and isinstance(entry.get("channel_id"), str):
            channels[entry["channel_id"]] = entry.get("team_id") if isinstance(entry.get("team_id"), str) else None
    for entry in message.get("threads") or []:
        if not isinstance(entry, dict) or not isinstance(entry.get("channel_id"), str) or not isinstance(entry.get("thread_ts"), str):
            continue
        since = entry.get("since") if isinstance(entry.get("since"), str) else None
        team = entry.get("team_id") if isinstance(entry.get("team_id"), str) else None
        threads[(entry["channel_id"], entry["thread_ts"])] = (team, since)
    return {"channels": channels, "threads": threads}


class Workspace:
    def __init__(self, session, http):
        self.token = session["token"]
        self.cookie = session["cookie"]
        self.team_id = session["team_id"]
        self.user_id = session["user_id"]
        self.team = session["team"]
        self.url = session["url"].rstrip("/") + "/"
        self.http = http
        self.users = {}
        self.channels = {}
        self.highlight_words = []
        self.prefs_at = None
        self.budget = Budget()

    def api(self, method, values=None):
        if method not in METHODS:
            raise SlackWatchError("slack_method_rejected")
        request = urllib.request.Request(
            f"https://slack.com/api/{method}",
            data=urllib.parse.urlencode(values or {}).encode(),
            headers={
                "Authorization": f"Bearer {self.token}",
                "Cookie": f"d={self.cookie}",
                "Content-Type": "application/x-www-form-urlencoded",
            },
            method="POST",
        )
        try:
            with self.http.open(request, timeout=20) as response:
                result = json.load(response)
        except urllib.error.HTTPError as error:
            if error.code == 429:
                raise RateLimited(int(error.headers.get("Retry-After") or 30)) from None
            raise SlackWatchError("slack_http_failed") from None
        except (urllib.error.URLError, TimeoutError, http.client.HTTPException, OSError):
            raise SlackWatchError("slack_network_failed") from None
        except (json.JSONDecodeError, UnicodeDecodeError):
            raise SlackWatchError("slack_response_invalid") from None
        if not isinstance(result, dict):
            raise SlackWatchError("slack_response_invalid")
        if not result.get("ok"):
            error = result.get("error")
            raise SlackWatchError(error if isinstance(error, str) and re.fullmatch(r"[a-z_]{1,64}", error) else "slack_request_failed")
        return result

    def user_name(self, user_id):
        if not user_id:
            return ""
        if user_id not in self.users:
            name = user_id
            try:
                user = self.api("users.info", {"user": user_id}).get("user") or {}
                profile = user.get("profile") or {}
                name = profile.get("display_name") or profile.get("real_name") or user.get("real_name") or user.get("name") or user_id
            except RateLimited:
                raise
            except SlackWatchError:
                pass
            self.users[user_id] = name
        return self.users[user_id]

    def channel_name(self, channel_id):
        if channel_id not in self.channels:
            name = channel_id
            try:
                channel = self.api("conversations.info", {"channel": channel_id}).get("channel") or {}
                if channel.get("is_im"):
                    name = self.user_name(channel.get("user"))
                else:
                    name = channel.get("name") or channel_id
            except RateLimited:
                raise
            except SlackWatchError:
                pass
            self.channels[channel_id] = name
        return self.channels[channel_id]

    def highlights(self):
        if self.prefs_at is None or time.monotonic() - self.prefs_at > PREFS_TTL:
            self.prefs_at = time.monotonic()
            try:
                prefs = self.api("users.prefs.get").get("prefs") or {}
                words = prefs.get("highlight_words") or ""
                self.highlight_words = [word.strip().lower() for word in words.split(",") if word.strip()]
            except RateLimited:
                raise
            except SlackWatchError:
                pass
        return self.highlight_words

    def readable(self, text):
        def mention(match):
            body = match.group(1)
            label = match.group(2)
            if body.startswith("@"):
                return "@" + (label or self.user_name(body[1:]))
            if body.startswith("#"):
                return "#" + (label or self.channel_name(body[1:]))
            if body.startswith("!subteam^"):
                return "@" + (label or "group").lstrip("@")
            if body.startswith("!"):
                return "@" + (label or body[1:].split("^")[0])
            return label or body

        return re.sub(r"<([^<>|]+)(?:\|([^<>]*))?>", mention, text or "").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")

    def author(self, message):
        if message.get("user"):
            return self.user_name(message["user"])
        profile = message.get("bot_profile") or {}
        return message.get("username") or profile.get("name") or "A Slack app"

    def permalink(self, channel_id, ts, thread_ts=None):
        link = f"{self.url}archives/{channel_id}/p{ts.replace('.', '')}"
        if thread_ts and thread_ts != ts:
            link += f"?thread_ts={thread_ts}&cid={channel_id}"
        return link

    def notification(self, kind, channel_id, message, channel_name=None):
        text = self.readable(message.get("text") or "")
        if not text.strip() and message.get("files"):
            names = ", ".join(f.get("name") or "a file" for f in message["files"][:3])
            text = f"shared {names}"
        thread_ts = message.get("thread_ts")
        return {
            "type": "notification",
            "kind": kind,
            "team_id": self.team_id,
            "team": self.team,
            "channel_id": channel_id,
            "channel_name": channel_name or self.channel_name(channel_id),
            "ts": message["ts"],
            "thread_ts": thread_ts if thread_ts and thread_ts != message["ts"] else None,
            "author": self.author(message),
            "author_id": message.get("user") or "",
            "text": text[:MAX_TEXT],
            "permalink": self.permalink(channel_id, message["ts"], thread_ts),
        }

    def from_someone_else(self, message):
        return (
            message.get("subtype") in MESSAGE_SUBTYPES
            and message.get("user") != self.user_id
            and isinstance(message.get("ts"), str)
            and (message.get("user") or message.get("bot_id"))
        )

    def classify(self, message):
        text = message.get("text") or ""
        if f"<@{self.user_id}>" in text or any(tag in text for tag in BROADCASTS):
            return "mention"
        lowered = text.lower()
        for word in self.highlights():
            if re.search(rf"(?<![\w]){re.escape(word)}(?![\w])", lowered):
                return "highlight"
        return None

    def history(self, channel_id, oldest):
        result = self.api("conversations.history", {"channel": channel_id, "oldest": oldest, "limit": 50, "inclusive": "false"})
        messages = [m for m in result.get("messages") or [] if isinstance(m, dict)]
        return sorted(messages, key=lambda m: ts_value(m.get("ts")))

    def mine(self, team_id):
        return team_id is None or team_id == self.team_id

    def poll(self, state, watches=None):
        watches = watches or {}
        watched = {channel for channel, team in (watches.get("channels") or {}).items() if self.mine(team)}
        found = []
        cursors = state.setdefault("cursors", {})
        started = state.setdefault("started", now_ts())
        counts = self.api("client.counts", {"thread_counts_by_channel": "true", "org_wide_aware": "true"})
        direct = [(entry, True) for key in ("ims", "mpims") for entry in counts.get(key) or []]
        rooms = [(entry, False) for entry in counts.get("channels") or []]
        member_of = set()
        for entry, is_direct in direct + rooms:
            if not isinstance(entry, dict) or not isinstance(entry.get("id"), str):
                continue
            channel_id = entry["id"]
            member_of.add(channel_id)
            latest = entry.get("latest") or ""
            cursor = cursors.get(channel_id) or started
            if not newer(latest, cursor):
                cursors.setdefault(channel_id, cursor)
                continue
            watching = not is_direct and channel_id in watched
            if not is_direct and not entry.get("mention_count") and not watching:
                cursors[channel_id] = latest
                continue
            if watching and not self.budget.take("conversations.history"):
                if not entry.get("mention_count"):
                    continue
                watching = False
            last_read = entry.get("last_read") or "0"
            for message in self.history(channel_id, cursor):
                if not newer(message.get("ts"), cursor) or not self.from_someone_else(message):
                    continue
                if not watching and not newer(message["ts"], last_read):
                    continue
                kind = "dm" if is_direct else self.classify(message)
                if watching and not kind:
                    kind = "watched"
                if kind:
                    found.append(self.notification(kind, channel_id, message))
            cursors[channel_id] = latest
        threads = counts.get("threads") or {}
        if threads.get("has_unreads") or threads.get("mention_count"):
            found.extend(self.thread_replies(state))
        found.extend(self.watched_threads(state, watches.get("threads") or {}, member_of))
        return found

    def watched_threads(self, state, threads, member_of):
        cursors = state.setdefault("thread_cursors", {})
        polled = state.setdefault("thread_polled", {})
        floor = f"{ts_value(state['started']) - THREAD_CATCH_UP:.6f}"
        found = []
        wanted = []
        for (channel_id, thread_ts), (team, since) in threads.items():
            if not self.mine(team) or channel_id not in member_of:
                continue
            key = f"{channel_id}|{thread_ts}"
            if key not in cursors:
                cursors[key] = max([thread_ts, floor, since or "0"], key=ts_value)
            if time.monotonic() - polled.get(key, float("-inf")) >= THREAD_EVERY:
                wanted.append((polled.get(key, float("-inf")), key, channel_id, thread_ts))
        live = {f"{c}|{t}" for c, t in threads}
        for key in [k for k in cursors if k not in live]:
            cursors.pop(key, None)
            polled.pop(key, None)
        for _, key, channel_id, thread_ts in sorted(wanted):
            if not self.budget.take("conversations.replies"):
                break
            polled[key] = time.monotonic()
            cursor = cursors[key]
            try:
                result = self.api("conversations.replies", {"channel": channel_id, "ts": thread_ts, "oldest": cursor, "limit": 50, "inclusive": "false"})
            except RateLimited:
                raise
            except SlackWatchError as error:
                LOGGER.debug("watched thread unavailable: %s", error)
                continue
            replies = sorted((m for m in result.get("messages") or [] if isinstance(m, dict)), key=lambda m: ts_value(m.get("ts")))
            for reply in replies:
                if not newer(reply.get("ts"), cursor):
                    continue
                cursors[key] = reply["ts"]
                if reply.get("ts") == thread_ts or not self.from_someone_else(reply):
                    continue
                reply.setdefault("thread_ts", thread_ts)
                found.append(self.notification(self.classify(reply) or "watched", channel_id, reply))
        return found

    def thread_replies(self, state):
        cursor = state.get("threads") or state["started"]
        newest = cursor
        found = []
        try:
            view = self.api("subscriptions.thread.getView", {"limit": 20, "current_ts": now_ts()})
        except RateLimited:
            raise
        except SlackWatchError as error:
            LOGGER.debug("thread view unavailable: %s", error)
            return found
        for thread in view.get("threads") or []:
            root = thread.get("root_msg") or {}
            channel_id = root.get("channel")
            if not isinstance(channel_id, str):
                continue
            for reply in thread.get("unread_replies") or []:
                if not isinstance(reply, dict) or not newer(reply.get("ts"), cursor) or not self.from_someone_else(reply):
                    continue
                reply.setdefault("thread_ts", root.get("ts"))
                found.append(self.notification("thread", channel_id, reply))
                if newer(reply["ts"], newest):
                    newest = reply["ts"]
        state["threads"] = newest
        return found


class Watcher:
    def __init__(self, interval):
        self.interval = interval
        self.http = _opener()
        self.workspaces = []
        self.state = {}
        self.watches = {}

    def listen(self, stream):
        for line in stream:
            try:
                message = json.loads(line)
            except (json.JSONDecodeError, UnicodeDecodeError):
                continue
            if isinstance(message, dict) and message.get("type") == "watches":
                self.watches = parse_watches(message)

    def connect(self):
        sessions = discover_sessions()
        self.workspaces = [Workspace(session, self.http) for session in sessions]
        for workspace in self.workspaces:
            self.state[workspace.team_id] = {"user_id": workspace.user_id, "started": now_ts(), "cursors": {}}
        emit(
            {
                "type": "status",
                "error": None,
                "workspaces": [{"team_id": w.team_id, "team": w.team} for w in self.workspaces],
            }
        )

    def step(self):
        for workspace in self.workspaces:
            for notification in workspace.poll(self.state[workspace.team_id], self.watches):
                emit(notification)

    def run(self):
        threading.Thread(target=self.listen, args=(sys.stdin,), daemon=True).start()
        backoff = self.interval
        while True:
            try:
                if not self.workspaces:
                    self.connect()
                self.step()
                backoff = self.interval
                time.sleep(self.interval)
            except RateLimited as limited:
                time.sleep(max(limited.retry_after, self.interval))
            except SlackSessionError as error:
                emit({"type": "status", "error": str(error), "workspaces": []})
                time.sleep(60)
            except SlackWatchError as error:
                if str(error) in {"invalid_auth", "token_revoked", "not_authed", "account_inactive"}:
                    self.workspaces = []
                emit({"type": "status", "error": str(error), "workspaces": [{"team_id": w.team_id, "team": w.team} for w in self.workspaces]})
                backoff = min(backoff * 2, 120)
                time.sleep(backoff)


def main():
    logging.basicConfig(level=logging.ERROR, format="%(message)s", stream=sys.stderr)
    key = os.environ.pop("WORLD_SLACK_CONNECTION_KEY", None)
    if not verify_connection_key(key, CONNECTION_PATH):
        emit({"type": "status", "error": "slack_connection_not_authorized", "workspaces": []})
        return 2
    try:
        interval = max(2.0, float(os.environ.get("WORLD_SLACK_POLL_SECS") or 4))
    except ValueError:
        interval = 4.0
    try:
        Watcher(interval).run()
    except KeyboardInterrupt:
        return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
