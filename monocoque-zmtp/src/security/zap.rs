//! ZeroMQ Authentication Protocol (ZAP) implementation
//!
//! ZAP is defined in RFC 27: <https://rfc.zeromq.org/spec/27/>
//!
//! ## Protocol Overview
//!
//! ZAP uses a REQ-REP pattern over inproc://zeromq.zap.01:
//! - Client (socket) sends authentication request
//! - Handler (user code) validates credentials and replies
//! - Socket accepts/rejects connection based on status code
//!
//! ## Message Format
//!
//! **Request** (multipart message):
//! 1. Version ("1.0")
//! 2. Request ID (unique per request)
//! 3. Domain (security domain)
//! 4. Address (peer IP address)
//! 5. Identity (ZMQ identity)
//! 6. Mechanism ("NULL", "PLAIN", "CURVE")
//! 7+. Credentials (mechanism-specific)
//!
//! **Response** (multipart message):
//! 1. Version ("1.0")
//! 2. Request ID (matches request)
//! 3. Status code ("200", "300", "400", "500")
//! 4. Status text (human-readable)
//! 5. User ID (authenticated user)
//! 6. Metadata (key-value pairs)

use bytes::Bytes;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counter used to generate unique ZAP request IDs.
///
/// Each call to `ZapRequest::new_with_unique_id` increments this counter so
/// that every request sent to the ZAP handler has a distinct ID, preventing
/// response correlation mistakes when multiple requests are in-flight.
static ZAP_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate the next unique ZAP request ID as a decimal string.
pub fn next_request_id() -> String {
    format!("{}", ZAP_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// ZAP version constant
pub const ZAP_VERSION: &str = "1.0";

/// ZAP endpoint for inproc transport
pub const ZAP_ENDPOINT: &str = "inproc://zeromq.zap.01";

/// Authentication mechanism
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZapMechanism {
    /// No authentication (NULL mechanism).
    Null,
    /// Username/password authentication (PLAIN mechanism).
    Plain,
    /// Public-key authentication (CURVE mechanism).
    Curve,
}

impl ZapMechanism {
    /// Return the wire-format mechanism name string.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Null => "NULL",
            Self::Plain => "PLAIN",
            Self::Curve => "CURVE",
        }
    }

    /// Parse a mechanism from its wire-format name string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "NULL" => Some(Self::Null),
            "PLAIN" => Some(Self::Plain),
            "CURVE" => Some(Self::Curve),
            _ => None,
        }
    }
}

/// ZAP status codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZapStatus {
    /// Success - connection accepted
    Success = 200,
    /// Temporary error - retry later
    TemporaryError = 300,
    /// Authentication failure
    Failure = 400,
    /// Internal error
    InternalError = 500,
}

impl ZapStatus {
    /// Return the numeric status code as a string.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "200",
            Self::TemporaryError => "300",
            Self::Failure => "400",
            Self::InternalError => "500",
        }
    }

    /// Parse a `ZapStatus` from its numeric string representation.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "200" => Some(Self::Success),
            "300" => Some(Self::TemporaryError),
            "400" => Some(Self::Failure),
            "500" => Some(Self::InternalError),
            _ => None,
        }
    }
}

/// ZAP authentication request
#[derive(Clone)]
pub struct ZapRequest {
    /// Version (always "1.0")
    pub version: String,
    /// Unique request ID
    pub request_id: String,
    /// Security domain
    pub domain: String,
    /// Peer address (IP:port)
    pub address: String,
    /// Peer identity
    pub identity: Bytes,
    /// Authentication mechanism
    pub mechanism: ZapMechanism,
    /// Mechanism-specific credentials
    pub credentials: Vec<Bytes>,
}

impl fmt::Debug for ZapRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZapRequest")
            .field("version", &self.version)
            .field("request_id", &self.request_id)
            .field("domain", &self.domain)
            .field("address", &self.address)
            .field("identity", &self.identity)
            .field("mechanism", &self.mechanism)
            .field(
                "credentials",
                &ZapRequestCredentialsDebug {
                    len: self.credentials.len(),
                },
            )
            .finish()
    }
}

struct ZapRequestCredentialsDebug {
    len: usize,
}

impl fmt::Debug for ZapRequestCredentialsDebug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.len == 0 {
            f.write_str("[]")
        } else {
            write!(f, "[<{} credential frame(s) redacted>]", self.len)
        }
    }
}
impl ZapRequest {
    /// Create a new ZAP request with a caller-supplied request ID.
    ///
    /// Prefer [`ZapRequest::new_with_unique_id`] in production code to ensure
    /// that every request has a distinct, monotonically increasing ID.
    pub fn new(
        request_id: impl Into<String>,
        domain: impl Into<String>,
        address: impl Into<String>,
        identity: Bytes,
        mechanism: ZapMechanism,
        credentials: Vec<Bytes>,
    ) -> Self {
        Self {
            version: ZAP_VERSION.to_string(),
            request_id: request_id.into(),
            domain: domain.into(),
            address: address.into(),
            identity,
            mechanism,
            credentials,
        }
    }

    /// Create a new ZAP request with an automatically generated unique request ID.
    ///
    /// The request ID is produced by a process-wide `AtomicU64` counter that
    /// starts at 1 and increments on every call.  This guarantees uniqueness
    /// within a process and makes it straightforward to correlate responses
    /// to their originating requests even when several ZAP round-trips are
    /// concurrent.
    pub fn new_with_unique_id(
        domain: impl Into<String>,
        address: impl Into<String>,
        identity: Bytes,
        mechanism: ZapMechanism,
        credentials: Vec<Bytes>,
    ) -> Self {
        Self::new(
            next_request_id(),
            domain,
            address,
            identity,
            mechanism,
            credentials,
        )
    }

    /// Encode request as multipart message
    pub fn encode(&self) -> Vec<Bytes> {
        let mut frames = vec![
            Bytes::from(self.version.clone()),
            Bytes::from(self.request_id.clone()),
            Bytes::from(self.domain.clone()),
            Bytes::from(self.address.clone()),
            self.identity.clone(),
            Bytes::from(self.mechanism.as_str()),
        ];
        frames.extend(self.credentials.clone());
        frames
    }

    /// Decode multipart message into request
    pub fn decode(frames: &[Bytes]) -> Result<Self, String> {
        if frames.len() < 6 {
            return Err("ZAP request requires at least 6 frames".to_string());
        }

        let version =
            String::from_utf8(frames[0].to_vec()).map_err(|_| "Invalid version string")?;
        if version != ZAP_VERSION {
            return Err("Unsupported ZAP request version".to_string());
        }
        let request_id = String::from_utf8(frames[1].to_vec()).map_err(|_| "Invalid request ID")?;
        let domain = String::from_utf8(frames[2].to_vec()).map_err(|_| "Invalid domain string")?;
        if domain.is_empty() {
            return Err("ZAP domain cannot be empty".to_string());
        }
        let address =
            String::from_utf8(frames[3].to_vec()).map_err(|_| "Invalid address string")?;
        if address.is_empty() {
            return Err("ZAP address cannot be empty".to_string());
        }
        let identity = frames[4].clone();
        if identity.len() > 255 {
            return Err("ZAP identity cannot exceed 255 bytes".to_string());
        }

        let mechanism_str =
            String::from_utf8(frames[5].to_vec()).map_err(|_| "Invalid mechanism string")?;
        let mechanism = ZapMechanism::from_str(&mechanism_str).ok_or("Unknown mechanism")?;

        let credentials = frames[6..].to_vec();
        let expected_credentials = match mechanism {
            ZapMechanism::Null => 0,
            ZapMechanism::Plain => 2,
            ZapMechanism::Curve => 1,
        };
        if credentials.len() != expected_credentials {
            return Err("ZAP credential count does not match mechanism".to_string());
        }

        Ok(Self {
            version,
            request_id,
            domain,
            address,
            identity,
            mechanism,
            credentials,
        })
    }
}

/// ZAP authentication response
#[derive(Debug, Clone)]
pub struct ZapResponse {
    /// Version (matches request)
    pub version: String,
    /// Request ID (matches request)
    pub request_id: String,
    /// Status code
    pub status_code: ZapStatus,
    /// Human-readable status text
    pub status_text: String,
    /// Authenticated user ID (empty if rejected)
    pub user_id: String,
    /// Optional metadata (RFC 35)
    pub metadata: HashMap<String, String>,
}

impl ZapResponse {
    /// Create a success response
    pub fn success(request_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            version: ZAP_VERSION.to_string(),
            request_id: request_id.into(),
            status_code: ZapStatus::Success,
            status_text: "OK".to_string(),
            user_id: user_id.into(),
            metadata: HashMap::new(),
        }
    }

    /// Create a failure response
    pub fn failure(request_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            version: ZAP_VERSION.to_string(),
            request_id: request_id.into(),
            status_code: ZapStatus::Failure,
            status_text: reason.into(),
            user_id: String::new(),
            metadata: HashMap::new(),
        }
    }

    /// Create an internal error response
    pub fn internal_error(request_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            version: ZAP_VERSION.to_string(),
            request_id: request_id.into(),
            status_code: ZapStatus::InternalError,
            status_text: reason.into(),
            user_id: String::new(),
            metadata: HashMap::new(),
        }
    }

    /// Encode response as multipart message
    pub fn encode(&self) -> Vec<Bytes> {
        // Encode metadata as key-value pairs (RFC 35 format)
        let metadata_bytes = if self.metadata.is_empty() {
            Bytes::new()
        } else {
            let mut buf = Vec::new();
            for (key, value) in &self.metadata {
                // RFC 35: key is 1-byte length-prefixed, so max 255 bytes.
                let key_bytes = key.as_bytes();
                let key_len = key_bytes.len().min(255);
                buf.push(key_len as u8);
                buf.extend_from_slice(&key_bytes[..key_len]);
                let value_len = (value.len() as u32).to_be_bytes();
                buf.extend_from_slice(&value_len);
                buf.extend_from_slice(value.as_bytes());
            }
            Bytes::from(buf)
        };

        vec![
            Bytes::from(self.version.clone()),
            Bytes::from(self.request_id.clone()),
            Bytes::from(self.status_code.as_str()),
            Bytes::from(self.status_text.clone()),
            Bytes::from(self.user_id.clone()),
            metadata_bytes,
        ]
    }

    /// Decode multipart message into response
    pub fn decode(frames: &[Bytes]) -> Result<Self, String> {
        if frames.len() != 6 {
            return Err(format!(
                "ZAP response requires 6 frames, got {}",
                frames.len()
            ));
        }

        let version =
            String::from_utf8(frames[0].to_vec()).map_err(|_| "Invalid version string")?;
        if version != ZAP_VERSION {
            return Err("Unsupported ZAP response version".to_string());
        }
        let request_id = String::from_utf8(frames[1].to_vec()).map_err(|_| "Invalid request ID")?;

        let status_str =
            String::from_utf8(frames[2].to_vec()).map_err(|_| "Invalid status code")?;
        let status_code = ZapStatus::from_str(&status_str).ok_or("Unknown status code")?;

        let status_text =
            String::from_utf8(frames[3].to_vec()).map_err(|_| "Invalid status text")?;
        if status_text.len() > 255 {
            return Err("ZAP status text cannot exceed 255 bytes".to_string());
        }
        if !frames[4].is_ascii() {
            return Err("Invalid user ID".to_string());
        }
        let user_id = String::from_utf8(frames[4].to_vec()).map_err(|_| "Invalid user ID")?;

        // Parse metadata (RFC 35 format)
        let metadata = Self::parse_metadata(&frames[5])?;

        Ok(Self {
            version,
            request_id,
            status_code,
            status_text,
            user_id,
            metadata,
        })
    }

    fn parse_metadata(data: &Bytes) -> Result<HashMap<String, String>, String> {
        let mut metadata = HashMap::new();
        if data.is_empty() {
            return Ok(metadata);
        }

        let mut cursor = 0;
        while cursor < data.len() {
            // Read key length (1 byte)
            if cursor >= data.len() {
                break;
            }
            let key_len = data[cursor] as usize;
            cursor += 1;

            // Read key
            if cursor + key_len > data.len() {
                return Err("Invalid metadata: key out of bounds".to_string());
            }
            let key = String::from_utf8(data[cursor..cursor + key_len].to_vec())
                .map_err(|_| "Invalid metadata key")?;
            cursor += key_len;

            // Read value length (4 bytes, big-endian)
            if cursor + 4 > data.len() {
                return Err("Invalid metadata: value length out of bounds".to_string());
            }
            let value_len = u32::from_be_bytes([
                data[cursor],
                data[cursor + 1],
                data[cursor + 2],
                data[cursor + 3],
            ]) as usize;
            cursor += 4;

            // Read value
            if cursor + value_len > data.len() {
                return Err("Invalid metadata: value out of bounds".to_string());
            }
            let value = String::from_utf8(data[cursor..cursor + value_len].to_vec())
                .map_err(|_| "Invalid metadata value")?;
            cursor += value_len;

            metadata.insert(key, value);
        }

        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zap_request_encode_decode() {
        let request = ZapRequest::new(
            "123",
            "test",
            "127.0.0.1:5555",
            Bytes::from("client1"),
            ZapMechanism::Plain,
            vec![Bytes::from("admin"), Bytes::from("password")],
        );

        let frames = request.encode();
        let decoded = ZapRequest::decode(&frames).unwrap();

        assert_eq!(decoded.version, ZAP_VERSION);
        assert_eq!(decoded.request_id, "123");
        assert_eq!(decoded.domain, "test");
        assert_eq!(decoded.mechanism, ZapMechanism::Plain);
        assert_eq!(decoded.credentials.len(), 2);
    }

    #[test]
    fn zap_request_decode_rejects_wrong_protocol_version() {
        let frames = vec![
            Bytes::from("0.9"),
            Bytes::from("123"),
            Bytes::from("test"),
            Bytes::from("127.0.0.1:5555"),
            Bytes::from("client1"),
            Bytes::from("PLAIN"),
            Bytes::from("admin"),
            Bytes::from("password"),
        ];

        assert!(
            ZapRequest::decode(&frames).is_err(),
            "ZAP accepted an authentication request with an unsupported protocol version"
        );
    }

    #[test]
    fn zap_request_decode_rejects_null_credentials() {
        let frames = vec![
            Bytes::from(ZAP_VERSION),
            Bytes::from("123"),
            Bytes::from("test"),
            Bytes::from("127.0.0.1"),
            Bytes::new(),
            Bytes::from("NULL"),
            Bytes::from("unexpected"),
        ];

        assert!(ZapRequest::decode(&frames).is_err());
    }

    #[test]
    fn zap_request_decode_rejects_empty_domain() {
        let frames = vec![
            Bytes::from(ZAP_VERSION),
            Bytes::from("123"),
            Bytes::new(),
            Bytes::from("127.0.0.1"),
            Bytes::new(),
            Bytes::from("NULL"),
        ];

        assert!(ZapRequest::decode(&frames).is_err());
    }

    #[test]
    fn zap_request_decode_rejects_empty_address() {
        let frames = vec![
            Bytes::from(ZAP_VERSION),
            Bytes::from("123"),
            Bytes::from("test"),
            Bytes::new(),
            Bytes::new(),
            Bytes::from("NULL"),
        ];

        assert!(ZapRequest::decode(&frames).is_err());
    }

    #[test]
    fn zap_request_decode_rejects_overlong_identity() {
        let frames = vec![
            Bytes::from(ZAP_VERSION),
            Bytes::from("123"),
            Bytes::from("test"),
            Bytes::from("127.0.0.1"),
            Bytes::from(vec![0u8; 256]),
            Bytes::from("NULL"),
        ];

        assert!(ZapRequest::decode(&frames).is_err());
    }

    #[test]
    fn zap_request_decode_rejects_plain_extra_credentials() {
        let frames = vec![
            Bytes::from(ZAP_VERSION),
            Bytes::from("123"),
            Bytes::from("test"),
            Bytes::from("127.0.0.1"),
            Bytes::new(),
            Bytes::from("PLAIN"),
            Bytes::from("admin"),
            Bytes::from("secret"),
            Bytes::from("shadow"),
        ];

        assert!(ZapRequest::decode(&frames).is_err());
    }

    #[test]
    fn zap_request_decode_rejects_curve_extra_credentials() {
        let frames = vec![
            Bytes::from(ZAP_VERSION),
            Bytes::from("123"),
            Bytes::from("test"),
            Bytes::from("127.0.0.1"),
            Bytes::new(),
            Bytes::from("CURVE"),
            Bytes::from(vec![0u8; 32]),
            Bytes::from("shadow"),
        ];

        assert!(ZapRequest::decode(&frames).is_err());
    }

    #[test]
    fn test_zap_response_success() {
        let response = ZapResponse::success("123", "testuser");
        let frames = response.encode();
        let decoded = ZapResponse::decode(&frames).unwrap();

        assert_eq!(decoded.status_code, ZapStatus::Success);
        assert_eq!(decoded.user_id, "testuser");
        assert_eq!(decoded.request_id, "123");
    }

    #[test]
    fn test_zap_response_failure() {
        let response = ZapResponse::failure("123", "Invalid credentials");
        let frames = response.encode();
        let decoded = ZapResponse::decode(&frames).unwrap();

        assert_eq!(decoded.status_code, ZapStatus::Failure);
        assert_eq!(decoded.status_text, "Invalid credentials");
        assert!(decoded.user_id.is_empty());
    }

    #[test]
    fn zap_response_decode_rejects_wrong_protocol_version() {
        let frames = vec![
            Bytes::from("0.9"),
            Bytes::from("123"),
            Bytes::from("200"),
            Bytes::from("OK"),
            Bytes::from("admin"),
            Bytes::new(),
        ];

        assert!(
            ZapResponse::decode(&frames).is_err(),
            "ZAP accepted an authentication success response with an unsupported protocol version"
        );
    }

    #[test]
    fn zap_response_decode_rejects_non_ascii_user_id() {
        let frames = vec![
            Bytes::from(ZAP_VERSION),
            Bytes::from("123"),
            Bytes::from("200"),
            Bytes::from("OK"),
            Bytes::from_static(b"jos\xc3\xa9"),
            Bytes::new(),
        ];

        assert!(ZapResponse::decode(&frames).is_err());
    }

    #[test]
    fn zap_response_decode_rejects_overlong_status_text() {
        let frames = vec![
            Bytes::from(ZAP_VERSION),
            Bytes::from("123"),
            Bytes::from("200"),
            Bytes::from("a".repeat(256)),
            Bytes::from("testuser"),
            Bytes::new(),
        ];

        assert!(ZapResponse::decode(&frames).is_err());
    }

    #[test]
    fn test_zap_metadata() {
        let mut response = ZapResponse::success("123", "admin");
        response
            .metadata
            .insert("role".to_string(), "superuser".to_string());
        response
            .metadata
            .insert("email".to_string(), "admin@example.com".to_string());

        let frames = response.encode();
        let decoded = ZapResponse::decode(&frames).unwrap();

        assert_eq!(decoded.metadata.len(), 2);
        assert_eq!(decoded.metadata.get("role"), Some(&"superuser".to_string()));
        assert_eq!(
            decoded.metadata.get("email"),
            Some(&"admin@example.com".to_string())
        );
    }
}
