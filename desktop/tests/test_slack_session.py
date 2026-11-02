import hashlib
import struct
import sys
from pathlib import Path

import pytest


sys.path.insert(0, str(Path(__file__).parents[1] / "scripts"))

import slack_session


def _varint(value):
    output = bytearray()
    while value >= 128:
        output.append((value & 127) | 128)
        value >>= 7
    output.append(value)
    return bytes(output)


def _block(key, value):
    entry = _varint(0) + _varint(len(key)) + _varint(len(value)) + key + value
    return entry + struct.pack("<II", 0, 1)


def _snappy_literal(value):
    length = len(value) - 1
    if length < 60:
        tag = bytes([length << 2])
    else:
        width = max(1, (length.bit_length() + 7) // 8)
        tag = bytes([(59 + width) << 2]) + length.to_bytes(width, "little")
    return _varint(len(value)) + tag + value


def test_extracts_value_from_snappy_sstable(tmp_path):
    token = b"xoxc-1234567890-abcdefghijklmnopqrstuvwxyz"
    data_block = _snappy_literal(_block(b"key", token))
    data_record = data_block + b"\x01\x00\x00\x00\x00"
    handle = _varint(0) + _varint(len(data_block))
    index_block = _block(b"index", handle)
    index_offset = len(data_record)
    index_record = index_block + b"\x00\x00\x00\x00\x00"
    handles = _varint(0) + _varint(0) + _varint(index_offset) + _varint(len(index_block))
    footer = handles.ljust(40, b"\x00") + bytes.fromhex("57fb808b247547db")
    path = tmp_path / "Local Storage/leveldb/table.ldb"
    path.parent.mkdir(parents=True)
    path.write_bytes(data_record + index_record + footer)

    assert slack_session._table_values(path) == [token]
    assert slack_session._find_tokens(tmp_path) == {token}


def test_cookie_host_digest_and_encoding_are_preserved():
    host = ".slack.com"
    cookie = b"xoxd-value%2Fstill-encoded"
    plaintext = hashlib.sha256(host.encode()).digest() + cookie

    assert slack_session._decode_cookie_plaintext(host, 24, plaintext) == cookie.decode()
    with pytest.raises(slack_session.SlackSessionError, match="cookie_host_mismatch: expected a cookie for slack.com, but it is bound to .slack.com") as raised:
        slack_session._decode_cookie_plaintext("slack.com", 24, plaintext)
    assert "xoxd" not in str(raised.value)


def test_rejects_authenticated_session_with_wrong_identity(monkeypatch, tmp_path):
    monkeypatch.setattr(slack_session, "_find_tokens", lambda root: {b"xoxc-1234567890-abcdefghijklmnopqrstuvwxyz"})
    monkeypatch.setattr(slack_session, "_read_cookie", lambda root: (".slack.com", "xoxd-cookie", b"", 24))
    monkeypatch.setattr(slack_session, "_opener", object)
    monkeypatch.setattr(
        slack_session,
        "_auth_test",
        lambda opener, token, cookie: {"ok": True, "team_id": "TOTHER", "user_id": "UOTHER"},
    )

    with pytest.raises(slack_session.SlackSessionError, match="slack_identity_mismatch"):
        slack_session.recover_session("TEXPECTED", "UEXPECTED", tmp_path)


def test_recovers_when_a_restart_leaves_two_valid_tokens(monkeypatch, tmp_path):
    monkeypatch.setattr(
        slack_session,
        "_find_tokens",
        lambda root: {b"xoxc-1111111111-aaaaaaaaaaaaaaaaaaaa", b"xoxc-2222222222-bbbbbbbbbbbbbbbbbbbb"},
    )
    monkeypatch.setattr(slack_session, "_read_cookie", lambda root: (".slack.com", "xoxd-cookie", b"", 24))
    monkeypatch.setattr(slack_session, "_opener", object)
    monkeypatch.setattr(
        slack_session,
        "_auth_test",
        lambda opener, token, cookie: {"ok": True, "team_id": "T1", "user_id": "U1"},
    )

    token, cookie = slack_session.recover_session("T1", "U1", tmp_path)

    assert token == "xoxc-1111111111-aaaaaaaaaaaaaaaaaaaa"
    assert cookie == "xoxd-cookie"


def test_slack_data_is_found_for_the_downloaded_and_the_app_store_app(tmp_path):
    with pytest.raises(slack_session.SlackSessionError, match="slack_app_missing"):
        slack_session.slack_data_root(tmp_path)
    store = tmp_path / "Library/Containers/com.tinyspeck.slackmacgap/Data/Library/Application Support/Slack"
    store.mkdir(parents=True)
    assert slack_session.slack_data_root(tmp_path) == store
    direct = tmp_path / "Library/Application Support/Slack"
    direct.mkdir(parents=True)
    assert slack_session.slack_data_root(tmp_path) == direct
