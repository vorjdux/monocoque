//! Message builder for ergonomic multipart message construction.
//!
//! Provides a fluent API for building ZeroMQ multipart messages with
//! automatic frame management.

use bytes::Bytes;
use std::io;

/// A multipart message builder with ergonomic frame construction.
///
/// # Examples
///
/// ```
/// use monocoque_core::message::Message;
///
/// // Build a simple message
/// let msg = Message::new()
///     .push_str("topic")
///     .push(b"data")
///     .into_frames();
/// assert_eq!(msg.len(), 2);
/// ```
///
/// ```
/// # use monocoque_core::message::Message;
/// // Build a message with JSON payload
/// #[derive(serde::Serialize)]
/// struct Data { value: u32 }
///
/// let msg = Message::new()
///     .push_str("events")
///     .push_json(&Data { value: 42 })
///     .unwrap()
///     .into_frames();
/// ```
#[derive(Debug, Clone, Default)]
pub struct Message {
    frames: Vec<Bytes>,
}

impl Message {
    /// Create a new empty message.
    #[must_use]
    pub const fn new() -> Self {
        Self { frames: Vec::new() }
    }

    /// Create a message from existing frames.
    #[must_use]
    pub const fn from_frames(frames: Vec<Bytes>) -> Self {
        Self { frames }
    }

    /// Add a frame from any type that can be converted to `Bytes`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use monocoque_core::message::Message;
    /// use bytes::Bytes;
    ///
    /// let msg = Message::new()
    ///     .push(b"raw bytes")
    ///     .push(vec![1, 2, 3])
    ///     .push(Bytes::from_static(b"static"));
    /// ```
    #[must_use]
    pub fn push(mut self, frame: impl Into<Bytes>) -> Self {
        self.frames.push(frame.into());
        self
    }

    /// Add a string frame (UTF-8 encoded).
    ///
    /// # Examples
    ///
    /// ```
    /// # use monocoque_core::message::Message;
    /// let msg = Message::new()
    ///     .push_str("topic")
    ///     .push_str("Hello, World!");
    /// ```
    #[must_use]
    pub fn push_str(mut self, s: &str) -> Self {
        self.frames.push(Bytes::copy_from_slice(s.as_bytes()));
        self
    }

    /// Add a frame with JSON-serialized data.
    ///
    /// Requires the `serde_json` crate to be available.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # use monocoque_core::message::Message;
    /// #[derive(serde::Serialize)]
    /// struct Event {
    ///     name: String,
    ///     value: i32,
    /// }
    ///
    /// let msg = Message::new()
    ///     .push_str("events")
    ///     .push_json(&Event {
    ///         name: "temperature".to_string(),
    ///         value: 23,
    ///     })
    ///     .unwrap();
    /// ```
    #[cfg(feature = "serde")]
    pub fn push_json<T: serde::Serialize>(mut self, value: &T) -> io::Result<Self> {
        let json = serde_json::to_vec(value)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.frames.push(Bytes::from(json));
        Ok(self)
    }

    /// Add an empty frame (useful for envelope separation).
    ///
    /// # Examples
    ///
    /// ```
    /// # use monocoque_core::message::Message;
    /// // ROUTER envelope: [identity] [empty] [data]
    /// let msg = Message::new()
    ///     .push(b"client-id")
    ///     .push_empty()
    ///     .push_str("request");
    /// ```
    #[must_use]
    pub fn push_empty(mut self) -> Self {
        self.frames.push(Bytes::new());
        self
    }

    /// Get the number of frames.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Check if the message has no frames.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Get a reference to the frames.
    #[must_use]
    pub fn frames(&self) -> &[Bytes] {
        &self.frames
    }

    /// Consume the builder and return the frames.
    #[must_use]
    pub fn into_frames(self) -> Vec<Bytes> {
        self.frames
    }

    /// Try to parse the first frame as JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame doesn't exist or isn't valid JSON.
    #[cfg(feature = "serde")]
    pub fn parse_frame_json<T: serde::de::DeserializeOwned>(&self, index: usize) -> io::Result<T> {
        let frame = self.frames.get(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Frame index out of bounds"))?;
        
        serde_json::from_slice(frame)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Try to parse the first frame as a UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame doesn't exist or isn't valid UTF-8.
    pub fn parse_frame_str(&self, index: usize) -> io::Result<&str> {
        let frame = self.frames.get(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Frame index out of bounds"))?;
        
        std::str::from_utf8(frame)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

impl From<Vec<Bytes>> for Message {
    fn from(frames: Vec<Bytes>) -> Self {
        Self { frames }
    }
}

impl From<Message> for Vec<Bytes> {
    fn from(msg: Message) -> Self {
        msg.frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_builder() {
        let msg = Message::new()
            .push(&b"frame1"[..])
            .push_str("frame2")
            .push_empty()
            .push(vec![1, 2, 3]);

        assert_eq!(msg.len(), 4);
        assert_eq!(msg.frames()[0], b"frame1"[..]);
        assert_eq!(msg.frames()[1], b"frame2"[..]);
        assert_eq!(msg.frames()[2], b""[..]);
        assert_eq!(msg.frames()[3], &[1, 2, 3][..]);
    }

    #[test]
    fn test_into_frames() {
        let frames = Message::new()
            .push_str("hello")
            .push_str("world")
            .into_frames();

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], b"hello"[..]);
        assert_eq!(frames[1], b"world"[..]);
    }

    #[test]
    fn test_parse_frame_str() {
        let msg = Message::new()
            .push_str("topic")
            .push(&b"data"[..]);

        assert_eq!(msg.parse_frame_str(0).unwrap(), "topic");
        assert!(msg.parse_frame_str(2).is_err()); // Out of bounds
    }

    #[cfg(feature = "serde")]
    #[test]
    fn test_json_serialization() {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct TestData {
            name: String,
            value: i32,
        }

        let data = TestData {
            name: "test".to_string(),
            value: 42,
        };

        let msg = Message::new()
            .push_str("topic")
            .push_json(&data)
            .unwrap();

        assert_eq!(msg.len(), 2);

        let parsed: TestData = msg.parse_frame_json(1).unwrap();
        assert_eq!(parsed, data);
    }
}
