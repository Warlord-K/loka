import ctypes
import hashlib
import hmac
import json
import os
import re
import sqlite3
import ssl
import stat
import struct
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path


TOKEN_PATTERN = re.compile(rb"xoxc-[A-Za-z0-9-]{10,}")
COOKIE_HOSTS = (".slack.com", "slack.com", "app.slack.com", ".app.slack.com")


class SlackSessionError(Exception):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, file_pointer, code, message, headers, new_url):
        return None


def _read_varint(data, offset):
    value = 0
    shift = 0
    while offset < len(data) and shift <= 63:
        byte = data[offset]
        offset += 1
        value |= (byte & 127) << shift
        if byte < 128:
            return value, offset
        shift += 7
    raise ValueError("invalid varint")


def _snappy_decompress(data):
    expected, offset = _read_varint(data, 0)
    output = bytearray()
    while offset < len(data) and len(output) < expected:
        tag = data[offset]
        offset += 1
        kind = tag & 3
        if kind == 0:
            length = tag >> 2
            if length < 60:
                length += 1
            else:
                width = length - 59
                if offset + width > len(data):
                    raise ValueError("invalid snappy literal")
                length = int.from_bytes(data[offset : offset + width], "little") + 1
                offset += width
            if offset + length > len(data):
                raise ValueError("invalid snappy literal")
            output.extend(data[offset : offset + length])
            offset += length
            continue
        if kind == 1:
            length = 4 + ((tag >> 2) & 7)
            if offset >= len(data):
                raise ValueError("invalid snappy copy")
            distance = ((tag & 224) << 3) | data[offset]
            offset += 1
        elif kind == 2:
            length = 1 + (tag >> 2)
            if offset + 2 > len(data):
                raise ValueError("invalid snappy copy")
            distance = int.from_bytes(data[offset : offset + 2], "little")
            offset += 2
        else:
            length = 1 + (tag >> 2)
            if offset + 4 > len(data):
                raise ValueError("invalid snappy copy")
            distance = int.from_bytes(data[offset : offset + 4], "little")
            offset += 4
        if not distance or distance > len(output):
            raise ValueError("invalid snappy distance")
        for _ in range(length):
            output.append(output[-distance])
    if len(output) != expected:
        raise ValueError("invalid snappy size")
    return bytes(output)


def _read_block(table, offset, size):
    if offset + size >= len(table):
        raise ValueError("invalid block handle")
    block = table[offset : offset + size]
    compression = table[offset + size]
    if compression == 0:
        return block
    if compression == 1:
        return _snappy_decompress(block)
    raise ValueError("unsupported block compression")


def _block_entries(block):
    if len(block) < 4:
        raise ValueError("invalid block")
    restart_count = struct.unpack_from("<I", block, len(block) - 4)[0]
    entries_end = len(block) - 4 * (restart_count + 1)
    if entries_end < 0:
        raise ValueError("invalid restart table")
    offset = 0
    previous = b""
    while offset < entries_end:
        shared, offset = _read_varint(block, offset)
        added, offset = _read_varint(block, offset)
        value_size, offset = _read_varint(block, offset)
        end = offset + added + value_size
        if shared > len(previous) or end > entries_end:
            raise ValueError("invalid block entry")
        key = previous[:shared] + block[offset : offset + added]
        value = block[offset + added : end]
        yield key, value
        previous = key
        offset = end


def _table_values(path):
    table = path.read_bytes()
    if len(table) < 48 or table[-8:] != bytes.fromhex("57fb808b247547db"):
        return []
    footer = table[-48:-8]
    _, offset = _read_varint(footer, 0)
    _, offset = _read_varint(footer, offset)
    index_offset, offset = _read_varint(footer, offset)
    index_size, _ = _read_varint(footer, offset)
    index = _read_block(table, index_offset, index_size)
    values = []
    for _, handle in _block_entries(index):
        block_offset, handle_offset = _read_varint(handle, 0)
        block_size, _ = _read_varint(handle, handle_offset)
        block = _read_block(table, block_offset, block_size)
        values.extend(value for _, value in _block_entries(block))
    return values


def _storage_locations(root):
    return (
        root / "Local Storage/leveldb",
        root / "IndexedDB/https_app.slack.com_0.indexeddb.leveldb",
        root / "Session Storage",
        root / "WebStorage",
    )


def _find_tokens(root):
    log_tokens = set()
    table_tokens = set()
    for location in _storage_locations(root):
        if not location.exists():
            continue
        for path in location.rglob("*"):
            if not path.is_file() or path.suffix not in (".ldb", ".log"):
                continue
            if path.suffix == ".log":
                log_tokens.update(TOKEN_PATTERN.findall(path.read_bytes()))
                continue
            try:
                values = _table_values(path)
            except ValueError:
                continue
            for value in values:
                table_tokens.update(TOKEN_PATTERN.findall(value))
    return {token for token in log_tokens | table_tokens if len(token) >= 30}


def _read_cookie(root):
    connection = sqlite3.connect(f"file:{root / 'Cookies'}?mode=ro", uri=True)
    try:
        version_row = connection.execute("select value from meta where key = 'version'").fetchone()
        version = int(version_row[0]) if version_row else 0
        chrome_now = int((time.time() + 11644473600) * 1_000_000)
        row = connection.execute(
            "select host_key, value, encrypted_value from cookies "
            "where name = 'd' and host_key in ('.slack.com', 'slack.com') and path = '/' "
            "and (expires_utc = 0 or expires_utc > ?) order by last_access_utc desc limit 1",
            (chrome_now,),
        ).fetchone()
        return (*row, version) if row else None
    finally:
        connection.close()


def _decode_cookie_plaintext(host, schema_version, plaintext):
    if schema_version >= 24:
        digest = hashlib.sha256(host.encode()).digest()
        if len(plaintext) < len(digest) or not hmac.compare_digest(plaintext[: len(digest)], digest):
            bound = next(
                (candidate for candidate in COOKIE_HOSTS if hashlib.sha256(candidate.encode()).digest() == plaintext[: len(digest)]),
                "an unknown host",
            )
            raise SlackSessionError(f"cookie_host_mismatch: expected a cookie for {host}, but it is bound to {bound}")
        plaintext = plaintext[len(digest) :]
    try:
        return plaintext.decode()
    except UnicodeDecodeError:
        raise SlackSessionError("cookie_decode_failed") from None


def _decrypt_cookie(host, encrypted, schema_version):
    try:
        result = subprocess.run(
            ["/usr/bin/security", "find-generic-password", "-w", "-s", "Slack Safe Storage"],
            capture_output=True,
            check=True,
            timeout=30,
        )
    except subprocess.CalledProcessError:
        raise SlackSessionError("keychain_lookup_failed") from None
    except subprocess.TimeoutExpired:
        raise SlackSessionError("keychain_lookup_timed_out") from None
    password = result.stdout.rstrip(b"\n")
    key = hashlib.pbkdf2_hmac("sha1", password, b"saltysalt", 1003, 16)
    ciphertext = encrypted[3:] if encrypted.startswith((b"v10", b"v11")) else encrypted
    try:
        library = ctypes.CDLL("/usr/lib/system/libcommonCrypto.dylib")
    except OSError:
        raise SlackSessionError("cookie_crypto_unavailable") from None
    library.CCCrypt.argtypes = [
        ctypes.c_uint,
        ctypes.c_uint,
        ctypes.c_uint,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.POINTER(ctypes.c_size_t),
    ]
    output = ctypes.create_string_buffer(len(ciphertext) + 16)
    moved = ctypes.c_size_t()
    status = library.CCCrypt(
        1,
        0,
        1,
        key,
        len(key),
        b" " * 16,
        ciphertext,
        len(ciphertext),
        output,
        len(output),
        ctypes.byref(moved),
    )
    if status:
        raise SlackSessionError("cookie_decryption_failed")
    return _decode_cookie_plaintext(host, schema_version, output.raw[: moved.value])


def _opener():
    return urllib.request.build_opener(
        NoRedirect,
        urllib.request.HTTPSHandler(context=ssl.create_default_context()),
    )


def _auth_test(opener, token, cookie):
    request = urllib.request.Request(
        "https://slack.com/api/auth.test",
        data=b"",
        headers={
            "Authorization": f"Bearer {token}",
            "Cookie": f"d={cookie}",
            "Content-Type": "application/x-www-form-urlencoded",
        },
        method="POST",
    )
    try:
        with opener.open(request, timeout=20) as response:
            result = json.load(response)
    except urllib.error.HTTPError as error:
        if 300 <= error.code < 400:
            raise SlackSessionError("redirect_rejected") from None
        raise SlackSessionError("slack_http_failed") from None
    except (urllib.error.URLError, TimeoutError):
        raise SlackSessionError("slack_network_failed") from None
    except (json.JSONDecodeError, UnicodeDecodeError):
        raise SlackSessionError("slack_response_invalid") from None
    return result if isinstance(result, dict) else {}


def _credentials(root):
    root = root or Path.home() / "Library/Application Support/Slack"
    try:
        candidates = _find_tokens(root)
        cookie_row = _read_cookie(root)
    except (OSError, sqlite3.Error):
        raise SlackSessionError("slack_storage_unavailable") from None
    if not candidates:
        raise SlackSessionError("slack_token_not_found")
    if not cookie_row:
        raise SlackSessionError("slack_cookie_not_found")
    host, value, encrypted, schema_version = cookie_row
    return candidates, value or _decrypt_cookie(host, encrypted, schema_version)


def discover_sessions(root=None):
    candidates, cookie = _credentials(root)
    opener = _opener()
    sessions = {}
    for candidate in sorted(candidates):
        try:
            token = candidate.decode()
        except UnicodeDecodeError:
            continue
        identity = _auth_test(opener, token, cookie)
        team_id = identity.get("team_id")
        user_id = identity.get("user_id")
        if not identity.get("ok") or not team_id or not user_id:
            continue
        sessions.setdefault(
            (team_id, user_id),
            {
                "token": token,
                "cookie": cookie,
                "team_id": team_id,
                "user_id": user_id,
                "team": identity.get("team") or team_id,
                "user": identity.get("user") or user_id,
                "url": identity.get("url") or "https://app.slack.com/",
            },
        )
    if not sessions:
        raise SlackSessionError("slack_not_signed_in")
    return list(sessions.values())


def verify_connection_key(key, path):
    try:
        parent = path.parent.stat()
        connection = path.stat()
        local_key = path.read_text().strip()
    except OSError:
        return False
    return bool(
        key
        and parent.st_uid == os.getuid()
        and connection.st_uid == os.getuid()
        and stat.S_IMODE(parent.st_mode) == 0o700
        and stat.S_IMODE(connection.st_mode) == 0o600
        and hmac.compare_digest(key, local_key)
    )


def recover_session(team_id, user_id, root=None):
    candidates, cookie = _credentials(root)
    opener = _opener()
    matches = []
    for candidate in sorted(candidates):
        try:
            token = candidate.decode()
        except UnicodeDecodeError:
            continue
        identity = _auth_test(opener, token, cookie)
        if identity.get("ok") and identity.get("team_id") == team_id and identity.get("user_id") == user_id:
            matches.append(token)
    if not matches:
        raise SlackSessionError("slack_identity_mismatch")
    return matches[0], cookie
