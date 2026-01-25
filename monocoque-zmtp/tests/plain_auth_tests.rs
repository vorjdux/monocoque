//! Integration tests for PLAIN authentication

use bytes::Bytes;
use monocoque_zmtp::security::plain::{PlainAuthHandler, StaticPlainHandler};
use monocoque_zmtp::security::zap::{ZapMechanism, ZapRequest, ZapStatus};

#[compio::test]
async fn test_static_plain_handler_valid_credentials() {
    let mut handler = StaticPlainHandler::new();
    handler.add_user("admin", "secret123");
    handler.add_user("guest", "guest123");

    // Valid admin credentials
    let result = handler
        .authenticate("admin", "secret123", "test", "127.0.0.1")
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "admin");

    // Valid guest credentials
    let result = handler
        .authenticate("guest", "guest123", "test", "127.0.0.1")
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "guest");
}

#[compio::test]
async fn test_static_plain_handler_invalid_password() {
    let mut handler = StaticPlainHandler::new();
    handler.add_user("admin", "secret123");

    let result = handler
        .authenticate("admin", "wrongpassword", "test", "127.0.0.1")
        .await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "Invalid password");
}

#[compio::test]
async fn test_static_plain_handler_unknown_user() {
    let mut handler = StaticPlainHandler::new();
    handler.add_user("admin", "secret123");

    let result = handler
        .authenticate("hacker", "anything", "test", "127.0.0.1")
        .await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "Unknown user");
}

#[test]
fn test_plain_zap_request_creation() {
    use monocoque_zmtp::security::plain::create_plain_zap_request;

    let request = create_plain_zap_request(
        "req-001",
        "production",
        "192.168.1.100:5555",
        Bytes::from("client-1"),
        "testuser",
        "testpass",
    );

    assert_eq!(request.version, "1.0");
    assert_eq!(request.request_id, "req-001");
    assert_eq!(request.domain, "production");
    assert_eq!(request.address, "192.168.1.100:5555");
    assert_eq!(request.identity, Bytes::from("client-1"));
    assert_eq!(request.mechanism, ZapMechanism::Plain);
    assert_eq!(request.credentials.len(), 2);
    assert_eq!(&request.credentials[0][..], b"testuser");
    assert_eq!(&request.credentials[1][..], b"testpass");
}

#[test]
fn test_plain_zap_request_encode_decode() {
    use monocoque_zmtp::security::plain::create_plain_zap_request;

    let original = create_plain_zap_request(
        "test-123",
        "api",
        "10.0.0.5:9999",
        Bytes::from("worker-42"),
        "alice",
        "wonderland",
    );

    // Encode to frames
    let frames = original.encode();
    assert_eq!(frames.len(), 8); // 6 base + 2 credentials

    // Decode back
    let decoded = ZapRequest::decode(&frames).unwrap();
    assert_eq!(decoded.version, original.version);
    assert_eq!(decoded.request_id, original.request_id);
    assert_eq!(decoded.domain, original.domain);
    assert_eq!(decoded.address, original.address);
    assert_eq!(decoded.identity, original.identity);
    assert_eq!(decoded.mechanism, original.mechanism);
    assert_eq!(decoded.credentials, original.credentials);
}

#[test]
fn test_plain_empty_credentials() {
    let mut handler = StaticPlainHandler::new();
    
    // Don't add any users
    let result = futures::executor::block_on(
        handler.authenticate("anyone", "anything", "test", "127.0.0.1")
    );
    assert!(result.is_err());
}

#[test]
fn test_plain_case_sensitive() {
    let mut handler = StaticPlainHandler::new();
    handler.add_user("Admin", "Secret123");

    // Wrong case username
    let result = futures::executor::block_on(
        handler.authenticate("admin", "Secret123", "test", "127.0.0.1")
    );
    assert!(result.is_err());

    // Wrong case password
    let result = futures::executor::block_on(
        handler.authenticate("Admin", "secret123", "test", "127.0.0.1")
    );
    assert!(result.is_err());

    // Correct case
    let result = futures::executor::block_on(
        handler.authenticate("Admin", "Secret123", "test", "127.0.0.1")
    );
    assert!(result.is_ok());
}
