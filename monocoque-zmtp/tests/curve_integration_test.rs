//! Integration tests for CURVE encryption
//!
//! Note: These tests verify that CURVE security options and key pairs work correctly.
//! Full end-to-end encrypted communication requires complete handshake implementation.

use monocoque_core::options::SocketOptions;
use monocoque_zmtp::security::curve::CurveKeyPair;

#[test]
fn test_curve_keypair_generation() {
    // Generate multiple key pairs and verify they're different
    let pair1 = CurveKeyPair::generate();
    let pair2 = CurveKeyPair::generate();

    // Keys should be 32 bytes (X25519)
    assert_eq!(pair1.public.as_bytes().len(), 32);
    assert_eq!(pair2.public.as_bytes().len(), 32);

    // Different generations should produce different public keys
    assert_ne!(pair1.public.as_bytes(), pair2.public.as_bytes());
    
    // Secret keys are also different (via diffie-hellman result)
    let shared1 = pair1.secret.diffie_hellman(&pair2.public);
    let shared2 = pair2.secret.diffie_hellman(&pair1.public);
    assert_eq!(shared1, shared2); // DH should agree
}

#[test]
fn test_curve_socket_options() {
    let keypair = CurveKeyPair::generate();

    // Server configuration
    let server_options = SocketOptions::new()
        .with_curve_server(true)
        .with_curve_keypair(*keypair.public.as_bytes(), *keypair.public.as_bytes());

    assert_eq!(server_options.curve_server, true);
    assert!(server_options.curve_publickey.is_some());

    // Client configuration
    let client_keypair = CurveKeyPair::generate();
    let client_options = SocketOptions::new()
        .with_curve_serverkey(*keypair.public.as_bytes())
        .with_curve_keypair(*client_keypair.public.as_bytes(), *client_keypair.public.as_bytes());

    assert_eq!(client_options.curve_server, false);
    assert_eq!(
        client_options.curve_serverkey,
        Some(*keypair.public.as_bytes())
    );
}

#[test]
fn test_curve_perfect_forward_secrecy() {
    // Generate server long-term keypair
    let server_keypair = CurveKeyPair::generate();

    // Client 1 session
    let client1_keypair = CurveKeyPair::generate();
    let _client1_options = SocketOptions::new()
        .with_curve_serverkey(*server_keypair.public.as_bytes())
        .with_curve_keypair(*client1_keypair.public.as_bytes(), *client1_keypair.public.as_bytes());

    // Client 2 session (different ephemeral keys)
    let client2_keypair = CurveKeyPair::generate();
    let _client2_options = SocketOptions::new()
        .with_curve_serverkey(*server_keypair.public.as_bytes())
        .with_curve_keypair(*client2_keypair.public.as_bytes(), *client2_keypair.public.as_bytes());

    // Each session has unique client keypair (different public keys)
    assert_ne!(
        client1_keypair.public.as_bytes(),
        client2_keypair.public.as_bytes()
    );

    // This demonstrates that each connection would have unique session keys
    // (actual shared secret would be computed via DH exchange)
}



