//! ZeroMQ security mechanisms
//!
//! Implements authentication and encryption per RFC 23 (PLAIN) and RFC 26 (CURVE).

pub mod curve;
pub mod plain;
pub mod zap;
/// ZAP client for sending authentication requests.
pub mod zap_client;
/// ZAP handler infrastructure and server implementation.
pub mod zap_handler;

pub use curve::{CurveKeyPair, CurvePublicKey, CurveSecretKey};
pub use plain::{PlainAuthHandler, PlainCredentials, StaticPlainHandler};
pub use zap::{ZAP_ENDPOINT, ZAP_VERSION, ZapMechanism, ZapRequest, ZapResponse, ZapStatus};
pub use zap_client::ZapClient;
pub use zap_handler::{
    DefaultZapHandler, ZapHandler, ZapServer, spawn_zap_server, start_default_zap_server,
};
