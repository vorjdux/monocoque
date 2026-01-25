//! ZeroMQ Authentication Protocol (ZAP) implementation
//!
//! ZAP is defined in RFC 27: https://rfc.zeromq.org/spec/27/
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

/// ZAP version constant
pub const ZAP_VERSION: &str = "1.0";

/// ZAP endpoint for inproc transport
pub const ZAP_ENDPOINT: &str = "inproc://zeromq.zap.01";

/// Authentication mechanism
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZapMechanism {
    Null,
    Plain,
    Curve,
}

impl ZapMechanism {
    pub fn as_str(&self) -> &'static str {
        match self {
            ZapMechanism::Null => "NULL",
            ZapMechanism::Plain => "PLAIN",
            ZapMechanism::Curve => "CURVE",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "NULL" => Some(ZapMechanism::Null),
            "PLAIN" => Some(ZapMechanism::Plain),
            "CURVE" => Some(ZapMechanism::Curve),
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
    pub fn as_str(&self) -> &'static str {
        match self {
            ZapStatus::Success => "200",
            ZapStatus::TemporaryError => "300",
            ZapStatus::Failure => "400",
            ZapStatus::InternalError => "500",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "200" => Some(ZapStatus::Success),
            "300" => Some(ZapStatus::TemporaryError),
            "400" => Some(ZapStatus::Failure),
            "500" => Some(ZapStatus::InternalError),
            _ => None,
        }
    }
}

/// ZAP authentication request
#[derive(Debug, Clone)]
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

impl ZapRequest {
    /// Create a new ZAP request
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

        let version = String::from_utf8(frames[0].to_vec())
            .map_err(|_| "Invalid version string")?;
        let request_id = String::from_utf8(frames[1].to_vec())
            .map_err(|_| "Invalid request ID")?;
        let domain = String::from_utf8(frames[2].to_vec())
            .map_err(|_| "Invalid domain string")?;
        let address = String::from_utf8(frames[3].to_vec())
            .map_err(|_| "Invalid address string")?;
        let identity = frames[4].clone();
        
        let mechanism_str = String::from_utf8(frames[5].to_vec())
            .map_err(|_| "Invalid mechanism string")?;
        let mechanism = ZapMechanism::from_str(&mechanism_str)
            .ok_or("Unknown mechanism")?;

        let credentials = frames[6..].to_vec();

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
                buf.push(key.len() as u8);
                buf.extend_from_slice(key.as_bytes());
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
            return Err(format!("ZAP response requires 6 frames, got {}", frames.len()));
        }

        let version = String::from_utf8(frames[0].to_vec())
            .map_err(|_| "Invalid version string")?;
        let request_id = String::from_utf8(frames[1].to_vec())
            .map_err(|_| "Invalid request ID")?;
        
        let status_str = String::from_utf8(frames[2].to_vec())
            .map_err(|_| "Invalid status code")?;
        let status_code = ZapStatus::from_str(&status_str)
            .ok_or("Unknown status code")?;
        
        let status_text = String::from_utf8(frames[3].to_vec())
            .map_err(|_| "Invalid status text")?;
        let user_id = String::from_utf8(frames[4].to_vec())
            .map_err(|_| "Invalid user ID")?;

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
    fn test_zap_metadata() {
        let mut response = ZapResponse::success("123", "admin");
        response.metadata.insert("role".to_string(), "superuser".to_string());
        response.metadata.insert("email".to_string(), "admin@example.com".to_string());

        let frames = response.encode();
        let decoded = ZapResponse::decode(&frames).unwrap();

        assert_eq!(decoded.metadata.len(), 2);
        assert_eq!(decoded.metadata.get("role"), Some(&"superuser".to_string()));
        assert_eq!(decoded.metadata.get("email"), Some(&"admin@example.com".to_string()));
    }
}
