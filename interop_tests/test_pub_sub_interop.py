#!/usr/bin/env python3
"""
PUB/SUB Interoperability Tests
"""

import zmq
import pytest
import time
import subprocess
import signal
from pathlib import Path


def test_libzmq_pub_to_monocoque_sub():
    """Test libzmq PUB → Monocoque SUB"""
    
    # Start Monocoque SUB subscriber
    subscriber = subprocess.Popen(
        ["cargo", "run", "--example", "sub_client", "--", "--port", "15560"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=Path(__file__).parent.parent
    )
    
    time.sleep(1)  # Let subscriber connect
    
    try:
        # Create libzmq PUB publisher
        ctx = zmq.Context()
        socket = ctx.socket(zmq.PUB)
        socket.bind("tcp://127.0.0.1:15560")
        
        time.sleep(0.5)  # Allow subscription to propagate
        
        # Publish messages
        for i in range(5):
            socket.send(f"Message {i}".encode())
            time.sleep(0.1)
        
        time.sleep(1)  # Allow messages to be received
        
        socket.close()
        ctx.term()
        
        # Check subscriber received messages
        subscriber.send_signal(signal.SIGTERM)
        stdout, _ = subscriber.communicate(timeout=5)
        
        # Verify output contains messages
        output = stdout.decode()
        assert "Message 0" in output
        assert "Message 4" in output
        
    finally:
        if subscriber.poll() is None:
            subscriber.kill()


def test_monocoque_pub_to_libzmq_sub():
    """Test Monocoque PUB → libzmq SUB"""
    
    # Start Monocoque PUB publisher
    publisher = subprocess.Popen(
        ["cargo", "run", "--example", "pub_server", "--", "--port", "15561"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=Path(__file__).parent.parent
    )
    
    time.sleep(1)  # Let publisher bind
    
    try:
        # Create libzmq SUB subscriber
        ctx = zmq.Context()
        socket = ctx.socket(zmq.SUB)
        socket.connect("tcp://127.0.0.1:15561")
        socket.setsockopt(zmq.SUBSCRIBE, b"")  # Subscribe to all
        
        time.sleep(0.5)  # Allow subscription to propagate
        
        # Receive messages
        received = []
        for _ in range(5):
            msg = socket.recv(flags=zmq.NOBLOCK if received else 0)
            received.append(msg)
        
        assert len(received) == 5
        assert b"Hello" in received[0]
        
        socket.close()
        ctx.term()
        
    finally:
        publisher.send_signal(signal.SIGTERM)
        publisher.wait(timeout=5)


def test_topic_filtering():
    """Test topic-based filtering"""
    
    # Start publisher
    ctx = zmq.Context()
    pub = ctx.socket(zmq.PUB)
    pub.bind("tcp://127.0.0.1:15562")
    
    time.sleep(0.5)
    
    # Start Monocoque subscriber with topic filter
    subscriber = subprocess.Popen(
        ["cargo", "run", "--example", "sub_client", "--", 
         "--port", "15562", "--topic", "ALERT"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=Path(__file__).parent.parent
    )
    
    time.sleep(1)
    
    try:
        # Publish messages with different topics
        pub.send(b"INFO: normal message")
        pub.send(b"ALERT: important message")
        pub.send(b"DEBUG: debug message")
        pub.send(b"ALERT: critical message")
        
        time.sleep(1)
        
        subscriber.send_signal(signal.SIGTERM)
        stdout, _ = subscriber.communicate(timeout=5)
        
        output = stdout.decode()
        
        # Should only receive ALERT messages
        assert "important message" in output
        assert "critical message" in output
        assert "normal message" not in output
        assert "debug message" not in output
        
    finally:
        pub.close()
        ctx.term()
        if subscriber.poll() is None:
            subscriber.kill()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
