Here is the implementation blueprint for the `PoisonGuard`.

This is not just a helper struct; it is a **structural guarantee** of protocol integrity. In an async system without background threads, this is the only thing standing between a `timeout` and a corrupted TCP stream.

### 1. The Implementation (`monocoque-core/src/poison.rs`)

This belongs in `monocoque-core` because it is protocol-agnostic. Any stateful I/O operation (ZMTP, HTTP, database protocols) needs this protection when running on a direct stream.

```rust
/// A RAII guard that protects against partial I/O corruption.
///
/// # The Problem
/// In async Rust, dropping a Future cancels the task immediately. If this happens
/// during a multi-step I/O operation (like writing a multipart ZMTP message),
/// the underlying stream is left in an undefined state (e.g., half a frame written).
///
/// # The Solution
/// 1. When `PoisonGuard::new` is called, we set the flag to `true` (assume failure).
/// 2. If the Future is dropped (cancelled) before completion, the guard is dropped,
///    and the flag remains `true`.
/// 3. Only if `disarm()` is strictly called (after I/O success) do we reset the flag to `false`.
pub struct PoisonGuard<'a> {
    flag: &'a mut bool,
}

impl<'a> PoisonGuard<'a> {
    /// Create a new guard. Sets the flag to `true` immediately.
    #[inline(always)]
    pub fn new(flag: &'a mut bool) -> Self {
        *flag = true;
        Self { flag }
    }

    /// Disarm the guard, resetting the flag to `false`.
    /// Call this ONLY when the critical section is fully complete.
    #[inline(always)]
    pub fn disarm(self) {
        *self.flag = false;
        // self is dropped here, but since we updated the reference,
        // the Drop impl (if we had one) doesn't need to do anything.
    }
}

```

---

### 2. Why This Is Non-Negotiable

You have built a high-performance engine that relies on **Zero-Copy** and **Direct I/O**.

-   **The Scenario:** You are sending a 1MB message.
-   **The Event:** The user puts a 5ms timeout on the request.
-   **The Failure:** The timeout fires after 500KB are written. The `flush()` future is dropped.
-   **The Aftermath:**
-   **Without PoisonGuard:** The socket looks "fine". The user retries sending a _new_ message. The peer receives the new header in the middle of the old payload. **Result:** Peer disconnects, "Protocol Error". Debugging this is a nightmare.
-   **With PoisonGuard:** The socket is marked `is_poisoned = true`. The next `send()` immediately returns `Error::ConnectionPoisoned`. The application knows the connection is dead and must reconnect.

### 3. How to Use It (The Pattern)

You must apply this pattern to **every single function** that performs non-atomic writes (i.e., writing more than one packet or a packet larger than the MTU).

**In `monocoque-zmtp/src/dealer.rs` (and others):**

```rust
pub struct DealerSocket<S> {
    stream: S,
    // Add this field
    is_poisoned: bool,
    // ...
}

impl<S> DealerSocket<S>
where S: AsyncWrite + Unpin
{
    pub async fn flush(&mut self) -> io::Result<()> {
        // STEP 1: Check health BEFORE doing anything
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket is poisoned due to previous cancelled I/O"
            ));
        }

        // STEP 2: Arm the guard.
        // If execution stops hereafter (panic, return, drop), 'is_poisoned' stays true.
        let guard = PoisonGuard::new(&mut self.is_poisoned);

        // STEP 3: The Danger Zone
        // If this await is cancelled, the guard drops and the socket remains poisoned.
        self.stream.write_all(&self.write_buffer).await?;

        // STEP 4: Success. We survived. Disarm.
        guard.disarm();

        Ok(())
    }
}

```

### 4. Mistakes to Avoid

#### ❌ Mistake 1: Disarming Too Early

```rust
let guard = PoisonGuard::new(&mut self.is_poisoned);
self.stream.write_all(&header).await?;
guard.disarm(); // WRONG!
self.stream.write_all(&body).await?; // If this fails/cancels, we are not protected!

```

**Rule:** Only disarm when the _entire_ logical message (all frames) has been flushed to the kernel.

#### ❌ Mistake 2: Ignoring the Error

If you catch `ConnectionPoisoned`, do not try to "reset" the flag manually.

```rust
if let Err(_) = socket.flush().await {
    socket.is_poisoned = false; // DANGEROUS!
    // You just told the system "this stream is clean" when it contains garbage.
}

```

**Rule:** Once poisoned, the `TcpStream` is radioactive. Drop it. Reconnect.

#### ❌ Mistake 3: Using on `read()` (Usually)

Reads are often idempotent or recoverable (depending on buffering). If you cancel a `read()`, you might just lose data you haven't processed yet, but you haven't necessarily corrupted the _stream state_ for the peer.

-   **Exception:** If you are reading a multipart message and reading frame 2 of 3 relies on internal state that gets reset on cancellation. In that case, poison the read side too.

### Next Step

This implementation is small but critical. Add it to `monocoque-core` now. It is the foundation for the **Automatic Reconnection** logic we will build in Phase 5.
