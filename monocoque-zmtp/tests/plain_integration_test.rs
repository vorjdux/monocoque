//! Integration tests for PLAIN authentication with REQ/REP flows
//!
//! Note: These tests verify that PLAIN security options are correctly applied.
//! Full end-to-end authentication requires ZAP handler integration which is
//! implemented but requires inproc transport setup.

use monocoque_core::options::SocketOptions;
use monocoque_zmtp::security::plain::StaticPlainHandler;
use monocoque_zmtp::security::PlainAuthHandler;
use std::time::Duration;

#[test]
fn test_plain_auth_options_configuration() {
    // Verify that PLAIN authentication options can be configured
    let server_options = SocketOptions::new()
        .with_plain_server(true)
        .with_recv_timeout(Duration::from_secs(5));

    assert_eq!(server_options.plain_server, true);
    assert!(server_options.plain_username.is_none());
    assert!(server_options.plain_password.is_none());

    let client_options = SocketOptions::new()
        .with_plain_credentials("testuser", "testpass");

    assert_eq!(client_options.plain_server, false);
    assert_eq!(client_options.plain_username, Some("testuser".to_string()));
    assert_eq!(client_options.plain_password, Some("testpass".to_string()));
}

#[test]
fn test_plain_handler_user_database() {
    compio::runtime::Runtime::new().unwrap().block_on(async {
        let mut handler = StaticPlainHandler::new();
        handler.add_user("alice", "secret1");
        handler.add_user("bob", "secret2");

        // Test valid credentials
        let result = handler
            .authenticate("alice", "secret1", "global", "127.0.0.1")
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "alice");

        // Test invalid password
        let result = handler
            .authenticate("alice", "wrong", "global", "127.0.0.1")
            .await;
        assert!(result.is_err());

        // Test unknown user
        let result = handler
            .authenticate("charlie", "anypass", "global", "127.0.0.1")
            .await;
        assert!(result.is_err());
    });
}

#[test]
fn test_plain_multiple_users() {
    compio::runtime::Runtime::new().unwrap().block_on(async {
        let mut handler = StaticPlainHandler::new();
        handler.add_user("user1", "pass1");
        handler.add_user("user2", "pass2");
        handler.add_user("user3", "pass3");

        // All users should authenticate successfully with correct passwords
        for (user, pass) in [("user1", "pass1"), ("user2", "pass2"), ("user3", "pass3")] {
            let result = handler.authenticate(user, pass, "global", "127.0.0.1").await;
            assert!(result.is_ok(), "User {} should authenticate", user);
        }

        // Cross-authentication should fail
        let result = handler.authenticate("user1", "pass2", "global", "127.0.0.1").await;
        assert!(result.is_err(), "Wrong password should fail");
    });
}
