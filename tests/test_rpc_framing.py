"""No-node regressions for how the test client frames coincubed RPC responses.

coincubed serializes each response straight onto the unbuffered Unix socket and
keeps the connection open for the next request, so one response can arrive in
any number of chunks of any size, with no delimiter and no EOF after it.
"""

import json
import socket

import pytest

from test_framework.utils import UnixDomainSocketRpc


class ScriptedSocket:
    """Deliver a response in the given chunks, the way recv() would.

    A chunk larger than the requested size is split, as a real recv() never
    returns more than it was asked for. Once the script is exhausted the peer
    either keeps the connection open (a further recv() would block, reported as
    a socket timeout) or has closed it (recv() returns b"").
    """

    def __init__(self, chunks, eof=False):
        self.chunks = [bytes(c) for c in chunks if c]
        self.eof = eof
        self.eof_reads = 0

    def recv(self, length):
        assert length > 0
        if self.chunks:
            chunk = self.chunks.pop(0)
            if len(chunk) > length:
                self.chunks.insert(0, chunk[length:])
                chunk = chunk[:length]
            return chunk
        if not self.eof:
            raise socket.timeout("peer is idle: the client would block here")
        self.eof_reads += 1
        # A client that ignores EOF would spin on b"" forever; fail instead.
        assert self.eof_reads < 3, "client kept reading after EOF"
        return b""


def response(result):
    return json.dumps({"jsonrpc": "2.0", "id": 0, "result": result}).encode()


def split(body, sizes):
    """Split body into chunks of the given sizes, then the remainder."""
    chunks, pos = [], 0
    for size in sizes:
        chunks.append(body[pos : pos + size])
        pos += size
    chunks.append(body[pos:])
    return chunks


def readobj(sock):
    return UnixDomainSocketRpc("unused")._readobj(sock)


# 2073 bytes, the size of the createspend response left unparsed in the CI
# failure (a 25-byte read, then a read of exactly 2048 bytes).
PSBT_BODY = response({"psbt": "x" * 2022})
assert len(PSBT_BODY) == 2073


@pytest.mark.parametrize(
    "sizes",
    [
        [],  # a single chunk
        [25, 2048],  # the CI failure: the second read fills the request exactly
        [3, 2048],
        [1, 2048],
        [2048],
        [1],
        [100],
        [1] * 40,
        [2072],
    ],
)
def test_parses_a_response_split_anywhere(sizes):
    sock = ScriptedSocket(split(PSBT_BODY, sizes))
    assert readobj(sock) == json.loads(PSBT_BODY)
    assert sock.chunks == []


def test_parses_a_response_delivered_byte_by_byte():
    body = response({"coins": [{"amount": i, "outpoint": "ab" * 32} for i in range(8)]})
    sock = ScriptedSocket([body[i : i + 1] for i in range(len(body))])
    assert readobj(sock) == json.loads(body)


def response_of_length(length):
    body = response({"pad": ""})
    body = response({"pad": "z" * (length - len(body))})
    assert len(body) == length
    return body


@pytest.mark.parametrize(
    "sizes",
    [
        # listcoins timed out with 2682 = 634 + 2048 bytes buffered.
        [634, 2048],
        # Every read after a short first one fills the request exactly:
        # 634, then 2048, then 2682 (the buffer size), then 5364.
        [634, 2048, 2682, 5364],
        [2047, 2048, 4095],
    ],
)
def test_parses_a_response_whose_last_read_is_exactly_full(sizes):
    body = response_of_length(sum(sizes))
    sock = ScriptedSocket(split(body, sizes))
    assert readobj(sock) == json.loads(body)
    assert sock.chunks == []


def test_parses_a_response_split_inside_a_multibyte_character():
    body = json.dumps(
        {"jsonrpc": "2.0", "id": 0, "result": {"label": "€" * 10}},
        ensure_ascii=False,
    ).encode()
    cut = body.index("€".encode()) + 1
    sock = ScriptedSocket([body[:cut], body[cut:]])
    assert readobj(sock) == json.loads(body)


def test_eof_after_a_partial_response_is_a_clear_error():
    sock = ScriptedSocket([PSBT_BODY[:25], PSBT_BODY[25:1000]], eof=True)
    with pytest.raises(ConnectionError, match="1000 bytes"):
        readobj(sock)


def test_eof_before_any_response_is_a_clear_error():
    sock = ScriptedSocket([], eof=True)
    with pytest.raises(ConnectionError, match="before sending a response"):
        readobj(sock)


def test_eof_right_after_a_complete_response_still_parses():
    sock = ScriptedSocket([PSBT_BODY[:25], PSBT_BODY[25:]], eof=True)
    assert readobj(sock) == json.loads(PSBT_BODY)
