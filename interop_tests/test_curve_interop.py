#!/usr/bin/env python3
"""
CURVE interoperability tests

Run monocoque's CURVE (RFC 26) against libzmq in both roles. pyzmq's wheels
bundle a libzmq built with CURVE, so these run even where the system libzmq
lacks it (and the Rust interop_curve tests skip).
"""

import binascii
import subprocess
import time
from pathlib import Path

import pytest
import zmq
from zmq.utils import z85

ROOT = Path(__file__).parent.parent
BINARY = ROOT / "target" / "debug" / "examples" / "curve_echo"

# One small frame, a multipart message with an empty frame and a frame larger
# than the read buffer, then a mid-sized one.
MESSAGES = [
    [b"a"],
    [b"x" * 70000, b"", b"tail"],
    [b"y" * 300],
]

pytestmark = pytest.mark.skipif(not zmq.has("curve"), reason="libzmq without CURVE")


def hex_key(z85_key):
    return binascii.hexlify(z85.decode(z85_key)).decode()


def echo_all(sock, messages):
    for frames in messages:
        sock.send_multipart(frames)
        assert sock.recv_multipart() == frames


def dealer(ctx):
    s = ctx.socket(zmq.DEALER)
    s.rcvtimeo = 10000
    s.linger = 0
    return s


def start_server(server_pub, server_sec, count, heartbeat_ms=None):
    cmd = [str(BINARY), "server", "tcp://127.0.0.1:0", hex_key(server_pub), hex_key(server_sec), str(count)]
    if heartbeat_ms:
        cmd.append(str(heartbeat_ms))
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True, cwd=ROOT)
    return proc, proc.stdout.readline().strip()


def test_libzmq_client_to_monocoque_server():
    server_pub, server_sec = zmq.curve_keypair()
    client_pub, client_sec = zmq.curve_keypair()
    proc, endpoint = start_server(server_pub, server_sec, len(MESSAGES))
    ctx = zmq.Context()
    try:
        s = dealer(ctx)
        s.curve_serverkey, s.curve_publickey, s.curve_secretkey = server_pub, client_pub, client_sec
        s.connect(endpoint)
        echo_all(s, MESSAGES)
        assert proc.wait(timeout=10) == 0
    finally:
        proc.kill()
        ctx.destroy()


def test_monocoque_client_to_libzmq_server():
    server_pub, server_sec = zmq.curve_keypair()
    client_pub, client_sec = zmq.curve_keypair()
    ctx = zmq.Context()
    s = dealer(ctx)
    s.curve_server = True
    s.curve_publickey, s.curve_secretkey = server_pub, server_sec
    port = s.bind_to_random_port("tcp://127.0.0.1")
    proc = subprocess.Popen(
        [str(BINARY), "client", f"tcp://127.0.0.1:{port}", hex_key(server_pub),
         hex_key(client_pub), hex_key(client_sec), str(len(MESSAGES))],
        cwd=ROOT,
    )
    try:
        assert s.recv() == b"hello"
        echo_all(s, MESSAGES)
        assert proc.wait(timeout=10) == 0
    finally:
        proc.kill()
        ctx.destroy()


def test_heartbeats_cross_curve_in_both_directions():
    """Both sides heartbeat every 50 ms. The PINGs and PONGs travel inside
    MESSAGE boxes; a connection that mishandled them would drop and the echo
    after the idle period would fail."""
    server_pub, server_sec = zmq.curve_keypair()
    client_pub, client_sec = zmq.curve_keypair()
    proc, endpoint = start_server(server_pub, server_sec, 2, heartbeat_ms=50)
    ctx = zmq.Context()
    try:
        s = dealer(ctx)
        s.curve_serverkey, s.curve_publickey, s.curve_secretkey = server_pub, client_pub, client_sec
        s.heartbeat_ivl = 50
        s.heartbeat_timeout = 200
        monitor = s.get_monitor_socket(zmq.EVENT_DISCONNECTED)
        s.connect(endpoint)
        echo_all(s, [[b"before"]])
        time.sleep(1.0)
        echo_all(s, [[b"after"]])
        assert not monitor.poll(0), "the CURVE connection dropped while heartbeating"
        assert proc.wait(timeout=10) == 0
    finally:
        proc.kill()
        ctx.destroy()
