#!/usr/bin/env python3
"""Simple standalone interop test"""

import zmq
import subprocess
import signal
import time
from pathlib import Path

BINARY = Path(__file__).parent / "target" / "debug" / "examples" / "simple_rep_server"

print("Starting Monocoque REP server...")
server = subprocess.Popen(
    [str(BINARY), "--port", "25555"],
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL
)

print(f"Server PID: {server.pid}")
time.sleep(1)

print("Creating libzmq REQ client...")
ctx = zmq.Context()
socket = ctx.socket(zmq.REQ)
socket.connect("tcp://127.0.0.1:25555")

print("Sending message...")
socket.send(b"Hello from libzmq")

print("Waiting for reply...")
reply = socket.recv()
print(f"Received: {reply}")

assert reply == b"Echo: Hello from libzmq", f"Expected 'Echo: Hello from libzmq', got '{reply}'"

print("✓ Test PASSED!")

socket.close()
ctx.term()

print("Stopping server...")
server.send_signal(signal.SIGTERM)
server.wait(timeout=2)

print("✓ All done!")
