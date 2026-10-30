# RFC compliance

What monocoque implements of each ZeroMQ RFC, where libzmq departs from the
text, and which side monocoque follows. Where the two disagree monocoque
follows libzmq, since interoperating with it is the point; each case is named.

Interop is tested against real libzmq: the Rust `interop_*` tests link the
system libzmq, and `interop_tests/` runs pyzmq (bundled libzmq 4.3.5, which has
CURVE) in CI.

---

## RFC 23 / 37: ZMTP 3.0 and 3.1

**Implemented:** the greeting, NULL/PLAIN/CURVE mechanisms, READY and ERROR,
short and long frames, MORE and COMMAND flags, PING/PONG (3.1), SUBSCRIBE and
CANCEL as commands (3.1), JOIN/LEAVE for RADIO/DISH.

- **Greeting sent in stages.** The RFC presents the 64-byte greeting as one
  unit. libzmq sends the 10-byte signature, then the major version once it has
  read the peer's signature, then the rest once it knows the peer's revision.
  monocoque does the same, so a ZMTP 1.0 or 2.0 peer is detected before it is
  sent bytes it cannot parse.
- **Signature padding.** The RFC says the eight padding bytes may be anything.
  libzmq puts its 1.0 identity frame length there so a 1.0 peer reads the
  signature as the start of a frame; monocoque does the same.
- **Subscriptions.** 3.1 defines SUBSCRIBE and CANCEL commands. libzmq 4.3
  still sends subscriptions as data frames with a leading `0x01`/`0x00` byte,
  and so does monocoque. Both forms are accepted on receive.
- **PING TTL and context.** A PING carries a 16-bit TTL in tenths of a second
  and up to 16 bytes of context, echoed in the PONG. A nonzero TTL tightens the
  local heartbeat timeout for that peer. Legacy (1.0/2.0) peers get no PINGs.
- **READY size.** The RFC sets no limit. monocoque refuses a READY larger than
  512 bytes during the handshake. Socket-Type, Identity and a few custom
  properties fit; a peer advertising many large properties is refused.
- **Socket-type pairs.** Mismatched pairs are refused with an ERROR command, as
  the RFC requires and as libzmq does.
- **Routing ids.** A ROUTER peer's Identity property of 1 to 255 bytes is used
  as its routing id; an empty one gets a generated 5-byte id starting with
  `0x00`, as in libzmq. An id from the peer that starts with `0x00` is
  replaced by a generated one, since that range is reserved for generated ids.
  A second peer claiming a live id is refused unless `router_handover` is set,
  as in libzmq.

## RFC 15 / 13: ZMTP 2.0 and 1.0 (legacy)

**Implemented**, matching libzmq's downgrade path.

- Revision 1 (2.0) peers exchange socket type numbers and identity frames,
  then use ZMTP 3 data framing with no commands.
- 1.0 peers and revision 0 use length-counts-flags framing.
- A publisher subscribes a 1.0 subscriber to everything, since 1.0 subscribers
  filter locally. This is libzmq's behaviour; the 1.0 text has no
  subscription upstream at all.
- **Security cannot be downgraded.** A socket using PLAIN or CURVE, or NULL
  with a ZAP domain, closes a 1.0 or 2.0 peer before reading past its
  greeting. libzmq does the same for mechanisms; monocoque applies it to ZAP as
  well.

## RFC 24: ZMTP-PLAIN

**Implemented**: HELLO, WELCOME, INITIATE, READY, with ZAP authentication on
the server.

- PLAIN sends credentials in the clear, as the RFC states. Use it on trusted
  networks or with CURVE.

## RFC 26: CurveZMQ, and RFC 25: ZMTP-CURVE

**Implemented** per the RFC, interoperating with libzmq in both roles
(client and server), with multipart messages, 70 KB frames and heartbeats.

- HELLO is 200 bytes: version 1.0, 72 bytes of anti-amplification padding,
  the client's short-term key, a nonce and the signature box.
- INITIATE's box carries the client's permanent key and a vouch, which boxes
  the client's short-term key and the server's long-term key.
- MESSAGE nonces strictly increase per direction and a repeat or decrease is
  refused. The client's first MESSAGE nonce is 3 and the server's is 2, as in
  libzmq, since the handshake used the earlier values.
- **MESSAGE as a data frame.** RFC 25 describes MESSAGE as a command. libzmq
  sends it as a data frame whose own flags byte, inside the box, says whether
  more frames follow. monocoque follows libzmq when sending and accepts both
  when receiving.
- **Commands after the handshake.** libzmq encrypts commands such as PING,
  PONG and JOIN inside MESSAGE boxes with inner flag `0x02`. monocoque does the same
  and refuses any command sent in the clear after the handshake, since it
  would be unauthenticated.
- Custom properties (`ZMQ_METADATA`) travel inside the INITIATE and READY
  boxes and become message metadata on the other side, as with libzmq.

## RFC 27: ZAP

**Implemented**: version 1.0 requests over `inproc://zeromq.zap.01`, with the
peer's IP address in the Address field for TCP connections, and 200/300/400/500
status codes.

- The handlers use native `async fn`, so a ZAP handler may do I/O.
- **Beyond libzmq:** after two failed authentications in a row from one IP,
  each new connection from it waits before its handshake starts, from 100 ms
  doubling up to 10 s. A success clears the address, and one quiet for a
  minute is forgotten. libzmq has no such delay.

## RFC 28: REQ/REP

**Implemented**: REQ alternates send and receive, adds the empty delimiter and
drops replies that do not match its outstanding request. REP keeps the
envelope and returns it with the reply.

- `req_relaxed` and `req_correlate` follow libzmq's `ZMQ_REQ_RELAXED` and
  `ZMQ_REQ_CORRELATE`.

## RFC 29: PUB/SUB

**Implemented**: prefix matching, subscription forwarding upstream, XPUB/XSUB,
`xpub_verbose`, `xpub_nodrop`, `invert_matching`.

- A PUB drops messages for a subscriber at its HWM, as the RFC and libzmq do.

## RFC 30: PIPELINE (PUSH/PULL)

**Implemented**: PUSH round-robins across peers with room, PULL fair-queues.

## RFC 31: PAIR

**Implemented**. Unlike libzmq, a PAIR socket is multi-peer at the transport
level like every other socket, but it only ever talks to the first peer that
connects; later connections are refused.

## RFC 41: CLIENT/SERVER, RFC 48: RADIO/DISH, SCATTER/GATHER, CHANNEL

**Implemented behind the `draft` feature.** All carry single-part messages: a
multipart send fails and a multipart message from a peer is dropped. They need
ZMTP 3.1, so 1.0 and 2.0 peers are refused.

- SERVER hands out a nonzero 32-bit routing id per peer.
- DISH joins exact group names of up to 255 bytes, as in libzmq.

## ZMQ_STREAM (no RFC)

Follows libzmq: `[identity, data]` on receive, `[identity, ""]` for connect and
disconnect notifications (`stream_notify`), and a send of `[identity, ""]`
closes that connection. Identities are 5 bytes: `0x00` and a 32-bit counter.

- Data a peer sent before closing is delivered before its disconnect
  notification, and a close requested with `[identity, ""]` writes everything
  queued first.
- Data and notifications carry the connection's `Peer-Address` metadata, as
  in libzmq.

## Transports

- `tcp://` and `ipc://` (Unix domain sockets) as in libzmq.
- `inproc://` passes `Msg` values with no encoding. It is bounded by the HWM
  in each direction and works across threads. Unlike libzmq, connecting before
  the bind fails rather than waiting.
- `pgm://`, `epgm://`, `norm://`, `tipc://` and `vmci://` are not supported.
