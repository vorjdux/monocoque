#!/usr/bin/env python3
"""Quick test: libzmq REQ -> Monocoque REP"""

import zmq
import time

# Create libzmq REQ socket
ctx = zmq.Context()
socket = ctx.socket(zmq.REQ)

print("Connecting to tcp://127.0.0.1:15555...")
socket.connect("tcp://127.0.0.1:15555")

print("Sending message...")
socket.send(b"Hello from libzmq")

print("Waiting for reply...")
reply = socket.recv()
print(f"Received: {reply}")

socket.close()
ctx.term()
