use crate::codec::ZmtpFrame;
use bytes::Bytes;

/// Errors produced by MultipartBuffer
#[derive(Debug)]
pub enum MultipartError {
    /// Message exceeded configured frame count
    TooManyFrames,
    /// Message exceeded configured byte size
    TooLarge,
}

/// Collects ZMTP frames until a complete multipart message is formed.
///
/// Invariants:
/// - Frames are appended in-order
/// - A message completes when `MORE == false`
/// - Limits are enforced eagerly to prevent DoS
///
/// This type is **NOT** thread-safe by design.
/// It is owned by a single Actor read-loop.
pub struct MultipartBuffer {
    frames: Vec<Bytes>,
    frame_count: usize,
    byte_count: usize,

    max_frames: usize,
    max_bytes: usize,
}

impl MultipartBuffer {
    /// Create a new buffer with limits.
    ///
    /// Example safe defaults:
    /// - max_frames = 128
    /// - max_bytes  = 8 * 1024 * 1024 (8 MiB)
    pub fn new(max_frames: usize, max_bytes: usize) -> Self {
        Self {
            frames: Vec::new(),
            frame_count: 0,
            byte_count: 0,
            max_frames,
            max_bytes,
        }
    }

    /// Push a frame into the buffer.
    ///
    /// Returns:
    /// - `Ok(None)` if the message is not complete
    /// - `Ok(Some(Vec<Bytes>))` if a full message was assembled
    /// - `Err(MultipartError)` on protocol or resource violation
    pub fn push_frame(&mut self, frame: ZmtpFrame) -> Result<Option<Vec<Bytes>>, MultipartError> {
        let payload = frame.payload.clone();

        // Enforce limits
        self.frame_count += 1;
        if self.frame_count > self.max_frames {
            self.reset();
            return Err(MultipartError::TooManyFrames);
        }

        self.byte_count += payload.len();
        if self.byte_count > self.max_bytes {
            self.reset();
            return Err(MultipartError::TooLarge);
        }

        self.frames.push(payload);

        if frame.more() {
            // Waiting for more frames
            Ok(None)
        } else {
            // Message complete
            let msg = std::mem::take(&mut self.frames);
            self.reset();
            Ok(Some(msg))
        }
    }

    #[inline]
    fn reset(&mut self) {
        self.frames.clear();
        self.frame_count = 0;
        self.byte_count = 0;
    }
}
