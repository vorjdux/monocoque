# Security Audit Preparation

**Version:** 0.1.0  
**Date:** January 25, 2026  
**Status:** Pre-Audit Documentation

---

## Executive Summary

This document prepares Monocoque for security audit by documenting:
- Threat model and attack surface
- Cryptographic implementations
- Known security considerations
- Potential vulnerabilities to investigate

**Security Posture:** Monocoque implements ZeroMQ security mechanisms (PLAIN, CURVE) with focus on:
- Memory safety (Rust)
- Authenticated encryption (CURVE/ChaCha20Poly1305)
- Protocol compliance (ZMTP RFC 23, RFC 26, RFC 27)

---

## Threat Model

### Trust Boundaries

```
┌─────────────────────────────────────────────────┐
│              Trusted Domain                     │
│  ┌──────────────┐         ┌──────────────┐     │
│  │ Application  │ ◄─────► │  Monocoque   │     │
│  │    Code      │  Safe   │   Library    │     │
│  └──────────────┘         └──────────────┘     │
│                                  │              │
└──────────────────────────────────┼──────────────┘
                                   │
                          ══════════════════
                          Network Boundary
                          ══════════════════
                                   │
┌──────────────────────────────────┼──────────────┐
│           Untrusted Domain       │              │
│                                  ▼              │
│                        ┌──────────────┐         │
│                        │   Network    │         │
│                        │  Adversary   │         │
│                        └──────────────┘         │
└─────────────────────────────────────────────────┘
```

### Threat Actors

1. **Network Attacker** (Passive)
   - Eavesdrop on unencrypted traffic (PLAIN)
   - Traffic analysis
   - Metadata collection

2. **Network Attacker** (Active)
   - Man-in-the-middle attacks
   - Message injection
   - Replay attacks
   - Protocol fuzzing

3. **Malicious Client**
   - Credential brute-force
   - Resource exhaustion
   - Protocol violation

4. **Compromised Peer**
   - Exploit after authentication
   - DoS via malformed messages

### Assets to Protect

| Asset | Confidentiality | Integrity | Availability |
|-------|----------------|-----------|--------------|
| Message payloads | CURVE only | ✅ All | ✅ All |
| Credentials (PLAIN) | ⚠️ Cleartext | N/A | N/A |
| Symmetric keys (CURVE) | ✅ Protected | ✅ Protected | N/A |
| Connection state | Low priority | ✅ Critical | ✅ Critical |

---

## Attack Surface

### 1. Network Protocol Parsing

**Components:**
- ZMTP handshake parser
- Frame decoder
- Multipart message assembler

**Risks:**
- Buffer overflows (mitigated by Rust)
- Integer overflows in length fields
- State machine confusion
- Malformed frame injection

**Mitigations:**
- Rust memory safety
- Bounds checking on all lengths
- State machine validation
- Maximum frame size limits

### 2. Authentication Mechanisms

#### PLAIN Mechanism

**Security Properties:**
- ❌ No encryption
- ❌ No integrity protection
- ✅ Simple authentication

**Attack Vectors:**
- Credential eavesdropping (cleartext)
- Replay attacks (no nonce)
- Credential brute-force

**Mitigations:**
- Document "localhost/trusted network only" requirement
- Recommend CURVE for production
- Rate limiting in ZAP handler (application-level)

#### CURVE Mechanism

**Security Properties:**
- ✅ Encryption (ChaCha20Poly1305)
- ✅ Authentication (X25519 ECDH)
- ✅ Forward secrecy (ephemeral keys)

**Attack Vectors:**
- Nonce reuse (if implementation flawed)
- Key compromise
- Side-channel attacks (timing)
- Cookie forgery

**Mitigations:**
- Nonce counter (no reuse)
- Secure key generation (OsRng)
- Constant-time operations (chacha20poly1305 crate)
- Cookie validation

### 3. ZAP Authentication Protocol

**Components:**
- ZAP client (server-side)
- ZAP handler (inproc endpoint)
- Request/response validation

**Risks:**
- Timeout bypass
- Status code manipulation
- Metadata injection
- Unauthorized access on ZAP failure

**Mitigations:**
- Timeout enforcement (default 5s)
- Type-safe status codes (enum)
- Metadata size limits
- Fail-closed on ZAP errors

---

## Cryptographic Implementation

### X25519 Key Exchange (CURVE)

**Library:** `x25519-dalek 2.0`

**Usage:**
```rust
// Key generation
let secret = StaticSecret::random_from_rng(OsRng);
let public = PublicKey::from(&secret);

// ECDH
let shared_secret = secret.diffie_hellman(&peer_public);
```

**Security Considerations:**
- ✅ Uses OS random number generator
- ✅ Constant-time operations
- ✅ Well-audited library
- ⚠️ Need to verify nonce handling

### ChaCha20Poly1305 AEAD (CURVE)

**Library:** `chacha20poly1305 0.10`

**Usage:**
```rust
let cipher = ChaCha20Poly1305::new(&key);
let ciphertext = cipher.encrypt(&nonce, plaintext)?;
let plaintext = cipher.decrypt(&nonce, ciphertext)?;
```

**Security Considerations:**
- ✅ Authenticated encryption
- ✅ Nonce counter prevents reuse
- ⚠️ Need to verify nonce increment logic
- ⚠️ Verify no nonce wrapping at u64::MAX

### Nonce Management

**Implementation:**
```rust
// Server/client maintain separate nonce counters
send_nonce: u64
recv_nonce: u64

// Nonce format: "CurveZMQ<direction><8-byte-counter>"
```

**Security Audit Items:**
- [ ] Verify nonce never reused
- [ ] Verify nonce counters increment correctly
- [ ] Verify nonce wrapping is handled (or prevented)
- [ ] Verify nonce format matches spec

---

## Timing Attack Analysis

### PLAIN Authentication

**Potential Timing Leaks:**

```rust
// POTENTIALLY VULNERABLE (example)
if password == expected_password {
    return Ok(user_id);
}
```

**Recommendation:** Use constant-time comparison

```rust
use subtle::ConstantTimeEq;

if password.as_bytes().ct_eq(expected_password.as_bytes()).into() {
    return Ok(user_id);
}
```

**Audit Action:** Review all credential comparisons in `plain.rs`

### CURVE Cookie Validation

**Current Implementation:** [TO BE REVIEWED]

**Audit Action:**
- [ ] Verify cookie validation is constant-time
- [ ] Check for early-exit on invalid cookies

---

## Known Security Considerations

### 1. PLAIN Mechanism Limitations

**Issue:** Credentials sent in cleartext

**Documentation:**
```rust
/// ⚠️ **SECURITY WARNING**
/// PLAIN sends credentials in cleartext. Only use:
/// - Localhost connections
/// - Encrypted transports (TLS, VPN)
/// - Trusted networks
```

**Mitigation:** Clear documentation, recommend CURVE for production

### 2. ZAP Timeout Bypass

**Issue:** If ZAP handler is unresponsive, authentication may timeout and fail-open

**Current Behavior:** Fail-closed (reject on timeout)

**Audit Action:**
- [ ] Verify all ZAP error paths reject connections
- [ ] Test timeout behavior

### 3. Resource Exhaustion

**Issue:** Large messages or high connection rate could exhaust memory

**Current Mitigations:**
- Buffer size limits (configurable)
- Per-socket memory allocation

**Audit Action:**
- [ ] Test with extremely large frames (1GB+)
- [ ] Test connection flood scenarios
- [ ] Review memory allocation patterns

### 4. Denial of Service

**Issue:** Malicious peer sends invalid frames repeatedly

**Current Mitigations:**
- Protocol errors close connection
- Invalid frames rejected

**Audit Action:**
- [ ] Fuzz protocol parser extensively
- [ ] Test recovery from corrupted state

---

## Security Testing Checklist

### Cryptography
- [ ] Verify X25519 key generation uses OsRng
- [ ] Verify ChaCha20Poly1305 nonces never reused
- [ ] Review nonce increment logic for overflows
- [ ] Test CURVE handshake against reference implementation
- [ ] Verify forward secrecy (ephemeral keys generated)

### Authentication
- [ ] Test PLAIN credential comparison for timing attacks
- [ ] Test ZAP timeout handling (fail-closed)
- [ ] Test ZAP status code validation
- [ ] Test credential injection attempts
- [ ] Test authentication bypass attempts

### Protocol
- [ ] Fuzz ZMTP decoder with malformed frames
- [ ] Test maximum frame size handling
- [ ] Test state machine with invalid sequences
- [ ] Test multipart message corruption
- [ ] Test protocol version mismatches

### Memory Safety
- [ ] Run under Valgrind/ASAN
- [ ] Test with large messages (1GB+)
- [ ] Test connection churn (rapid connect/disconnect)
- [ ] Review all unsafe blocks (if any)
- [ ] Test concurrent socket access

### Network
- [ ] Test man-in-the-middle scenarios
- [ ] Test replay attack detection
- [ ] Test connection hijacking attempts
- [ ] Test resource exhaustion (connection flood)

---

## Audit Scope

### In Scope
- ZMTP protocol implementation
- PLAIN authentication mechanism
- CURVE encryption mechanism
- ZAP protocol implementation
- Codec (frame encoding/decoding)
- Handshake logic

### Out of Scope
- Application logic using Monocoque
- Network stack (TCP/IP)
- Operating system security
- Physical security

---

## Recommendations for Auditors

1. **Focus on cryptographic nonce handling** - Most critical for CURVE security
2. **Review timing attack surfaces** - Especially PLAIN credential comparison
3. **Fuzz protocol parser** - Use provided fuzz targets
4. **Test ZAP failure modes** - Verify fail-closed behavior
5. **Review memory allocation** - Look for unbounded allocations

---

## References

- RFC 23: ZMTP PLAIN Mechanism - https://rfc.zeromq.org/spec/23/
- RFC 26: CurveZMQ Mechanism - https://rfc.zeromq.org/spec/26/
- RFC 27: ZAP Authentication Protocol - https://rfc.zeromq.org/spec/27/
- X25519 RFC 7748 - https://www.rfc-editor.org/rfc/rfc7748
- ChaCha20-Poly1305 RFC 8439 - https://www.rfc-editor.org/rfc/rfc8439

---

## Post-Audit Actions

After security audit:
- [ ] Address all critical findings
- [ ] Address high-priority findings
- [ ] Document any accepted risks
- [ ] Update security documentation
- [ ] Publish security advisory if needed
