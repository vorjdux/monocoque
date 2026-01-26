//! Ergonomic message builder for constructing `ZeroMQ` multipart messages.
//!
//! This module provides a fluent API for building multipart messages with
//! automatic frame handling and type conversions.

use bytes::Bytes;

/// Builder for constructing `ZeroMQ` multipart messages.
///
/// Provides a fluent API for adding frames to a message with automatic
/// conversions from common types (strings, bytes, JSON, etc.).
///
/// # Examples
///
/// ```
/// use monocoque_core::message_builder::Message;
///
/// // Simple text frames
/// let msg = Message::new()
///     .push_str("topic")
///     .push_str("Hello, World!")
///     .into_frames();
///
/// // Mixed types
/// let msg = Message::new()
///     .push(b"routing_id")
///     .push_str("command:execute")
///     .push(&[1, 2, 3, 4])
///     .into_frames();
/// ```
///
/// ## With Serde JSON (optional)
///
/// ```ignore
/// #[derive(Serialize)]
/// struct Task {
///     id: u64,
///     name: String,
/// }
///
/// let task = Task { id: 42, name: "Process data".to_string() };
/// let msg = Message::new()
///     .push_str("tasks")
///     .push_json(&task)?
///     .into_frames();
/// ```
#[derive(Debug, Clone, Default)]
pub struct Message {
    frames: Vec<Bytes>,
}

impl Message {
    /// Create a new empty message builder.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::message_builder::Message;
    ///
    /// let msg = Message::new();
    /// ```
    #[must_use] 
    pub const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    /// Create a message with pre-allocated capacity.
    ///
    /// Useful when you know the number of frames in advance to avoid reallocations.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::message_builder::Message;
    ///
    /// let msg = Message::with_capacity(4)
    ///     .push_str("frame1")
    ///     .push_str("frame2")
    ///     .push_str("frame3")
    ///     .push_str("frame4");
    /// ```
    #[must_use] 
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            frames: Vec::with_capacity(capacity),
        }
    }

    /// Add a frame from any type that can be converted to `Bytes`.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::message_builder::Message;
    /// use bytes::Bytes;
    ///
    /// let msg = Message::new()
    ///     .push(b"raw bytes")
    ///     .push(vec![1, 2, 3])
    ///     .push(Bytes::from_static(b"static"));
    /// ```
    pub fn push(mut self, frame: impl Into<Bytes>) -> Self {
        self.frames.push(frame.into());
        self
    }

    /// Add a string frame.
    ///
    /// Convenience method for adding UTF-8 strings.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::message_builder::Message;
    ///
    /// let msg = Message::new()
    ///     .push_str("Hello")
    ///     .push_str("World");
    /// ```
    #[must_use] 
    pub fn push_str(mut self, s: &str) -> Self {
        self.frames.push(Bytes::copy_from_slice(s.as_bytes()));
        self
    }

    /// Add an empty frame.
    ///
    /// Empty frames are often used as delimiters in `ZeroMQ` envelope patterns.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::message_builder::Message;
    ///
    /// // ROUTER envelope: [identity, empty, body]
    /// let msg = Message::new()
    ///     .push(b"client-123")
    ///     .push_empty()
    ///     .push_str("Hello");
    /// ```
    #[must_use] 
    pub fn push_empty(mut self) -> Self {
        self.frames.push(Bytes::new());
        self
    }

    /// Add a frame containing JSON-serialized data.
    ///
    /// Requires the `serde` feature to be enabled.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use monocoque_core::message_builder::Message;
    /// use serde::Serialize;
    ///
    /// #[derive(Serialize)]
    /// struct Data {
    ///     value: i32,
    /// }
    ///
    /// let data = Data { value: 42 };
    /// let msg = Message::new()
    ///     .push_str("data")
    ///     .push_json(&data)?
    ///     .into_frames();
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "serde")]
    pub fn push_json<T: serde::Serialize>(mut self, value: &T) -> Result<Self, serde_json::Error> {
        let json = serde_json::to_vec(value)?;
        self.frames.push(Bytes::from(json));
        Ok(self)
    }

    /// Add a frame containing MessagePack-serialized data.
    ///
    /// Requires the `msgpack` feature to be enabled. MessagePack is more compact
    /// than JSON and often preferred for binary protocols.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    #[cfg(feature = "msgpack")]
    pub fn push_msgpack<T: serde::Serialize>(
        mut self,
        value: &T,
    ) -> Result<Self, rmp_serde::encode::Error> {
        let msgpack = rmp_serde::to_vec(value)?;
        self.frames.push(Bytes::from(msgpack));
        Ok(self)
    }

    /// Add a frame containing a big-endian u32.
    ///
    /// Useful for protocol headers, message IDs, etc.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::message_builder::Message;
    ///
    /// let msg = Message::new()
    ///     .push_u32(12345) // Message ID
    ///     .push_str("payload");
    /// ```
    #[must_use] 
    pub fn push_u32(mut self, value: u32) -> Self {
        self.frames.push(Bytes::copy_from_slice(&value.to_be_bytes()));
        self
    }

    /// Add a frame containing a big-endian u64.
    #[must_use] 
    pub fn push_u64(mut self, value: u64) -> Self {
        self.frames.push(Bytes::copy_from_slice(&value.to_be_bytes()));
        self
    }

    /// Get the number of frames in the message.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::message_builder::Message;
    ///
    /// let msg = Message::new()
    ///     .push_str("frame1")
    ///     .push_str("frame2");
    ///
    /// assert_eq!(msg.len(), 2);
    /// ```
    #[must_use] 
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Check if the message is empty (has no frames).
    #[must_use] 
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Consume the builder and return the frames as a `Vec<Bytes>`.
    ///
    /// This is the final step to get the message ready for sending.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use monocoque_core::message_builder::Message;
    /// # async fn example() -> std::io::Result<()> {
    /// # use bytes::Bytes;
    /// # let mut socket = monocoque_zmtp::DealerSocket::<compio::net::TcpStream>::new();
    ///
    /// let msg = Message::new()
    ///     .push_str("Hello")
    ///     .push_str("World")
    ///     .into_frames();
    ///
    /// socket.send(msg).await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use] 
    pub fn into_frames(self) -> Vec<Bytes> {
        self.frames
    }

    /// Get a reference to the frames without consuming the builder.
    #[must_use] 
    pub fn frames(&self) -> &[Bytes] {
        &self.frames
    }

    /// Create a message from existing frames.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::message_builder::Message;
    /// use bytes::Bytes;
    ///
    /// let frames = vec![
    ///     Bytes::from_static(b"frame1"),
    ///     Bytes::from_static(b"frame2"),
    /// ];
    ///
    /// let msg = Message::from_frames(frames);
    /// assert_eq!(msg.len(), 2);
    /// ```
    #[must_use] 
    pub const fn from_frames(frames: Vec<Bytes>) -> Self {
        Self { frames }
    }
}

impl From<Vec<Bytes>> for Message {
    fn from(frames: Vec<Bytes>) -> Self {
        Self::from_frames(frames)
    }
}

impl From<Message> for Vec<Bytes> {
    fn from(msg: Message) -> Self {
        msg.into_frames()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_message() {
        let msg = Message::new();
        assert_eq!(msg.len(), 0);
        assert!(msg.is_empty());
    }

    #[test]
    fn test_build_message() {
        let msg = Message::new()
            .push_str("topic")
            .push_str("Hello")
            .push(Vec::from(&b"World"[..]));

        assert_eq!(msg.len(), 3);
        assert!(!msg.is_empty());

        let frames = msg.into_frames();
        assert_eq!(frames[0], Bytes::from_static(b"topic"));
        assert_eq!(frames[1], Bytes::from_static(b"Hello"));
        assert_eq!(frames[2], Bytes::from_static(b"World"));
    }

    #[test]
    fn test_push_empty() {
        let msg = Message::new()
            .push(Vec::from(&b"id"[..]))
            .push_empty()
            .push_str("body");

        let frames = msg.into_frames();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1].len(), 0);
    }

    #[test]
    fn test_push_integers() {
        let msg = Message::new()
            .push_u32(12345)
            .push_u64(67890);

        let frames = msg.into_frames();
        assert_eq!(frames[0].len(), 4);
        assert_eq!(frames[1].len(), 8);

        let val32 = u32::from_be_bytes(frames[0].as_ref().try_into().unwrap());
        assert_eq!(val32, 12345);

        let val64 = u64::from_be_bytes(frames[1].as_ref().try_into().unwrap());
        assert_eq!(val64, 67890);
    }

    #[test]
    fn test_with_capacity() {
        let msg = Message::with_capacity(10);
        assert_eq!(msg.len(), 0);
        assert!(msg.frames.capacity() >= 10);
    }

    #[test]
    fn test_from_frames() {
        let frames = vec![
            Bytes::from_static(b"a"),
            Bytes::from_static(b"b"),
        ];
        let msg = Message::from_frames(frames.clone());
        assert_eq!(msg.len(), 2);
        assert_eq!(msg.frames(), &frames[..]);
    }
}
