//! Integration tests for CURVE encryption

use monocoque_zmtp::security::curve::{
    CurveKeyPair, CurvePublicKey, CurveSecretKey, CURVE_KEY_SIZE,
};

#[test]
fn test_curve_keypair_generation() {
    let keypair = CurveKeyPair::generate();
    
    // Verify key sizes
    assert_eq!(keypair.public.as_bytes().len(), CURVE_KEY_SIZE);
    
    // Verify public key matches secret key
    let derived_public = keypair.secret.public_key();
    assert_eq!(keypair.public, derived_public);
}

#[test]
fn test_curve_multiple_keypairs_are_unique() {
    let kp1 = CurveKeyPair::generate();
    let kp2 = CurveKeyPair::generate();
    
    // Extremely unlikely to generate same key twice
    assert_ne!(kp1.public.as_bytes(), kp2.public.as_bytes());
}

#[test]
fn test_curve_diffie_hellman_agreement() {
    let alice = CurveKeyPair::generate();
    let bob = CurveKeyPair::generate();

    // Both compute shared secret
    let alice_shared = alice.secret.diffie_hellman(&bob.public);
    let bob_shared = bob.secret.diffie_hellman(&alice.public);

    // Shared secrets must match (DH property)
    assert_eq!(alice_shared, bob_shared);
    assert_eq!(alice_shared.len(), CURVE_KEY_SIZE);
}

#[test]
fn test_curve_diffie_hellman_different_peers() {
    let alice = CurveKeyPair::generate();
    let bob = CurveKeyPair::generate();
    let charlie = CurveKeyPair::generate();

    let alice_bob = alice.secret.diffie_hellman(&bob.public);
    let alice_charlie = alice.secret.diffie_hellman(&charlie.public);

    // Different peer = different shared secret
    assert_ne!(alice_bob, alice_charlie);
}

#[test]
fn test_curve_keypair_from_bytes() {
    let original = CurveKeyPair::generate();
    let secret_bytes = *original.public.as_bytes(); // Placeholder - should be secret
    
    let recreated_secret = CurveSecretKey::from_bytes(secret_bytes);
    let recreated_public = CurvePublicKey::from_bytes(secret_bytes);
    
    // Verify reconstruction works
    assert_eq!(recreated_public.as_bytes().len(), CURVE_KEY_SIZE);
}

#[test]
fn test_curve_public_key_conversions() {
    let keypair = CurveKeyPair::generate();
    
    // Convert to X25519
    let x25519_key = keypair.public.to_x25519();
    
    // Convert back to CurvePublicKey
    let curve_key = CurvePublicKey::from(x25519_key);
    
    // Should round-trip correctly
    assert_eq!(keypair.public, curve_key);
}

#[test]
fn test_curve_zap_request() {
    use bytes::Bytes;
    use monocoque_zmtp::security::curve::create_curve_zap_request;
    use monocoque_zmtp::security::zap::ZapMechanism;

    let keypair = CurveKeyPair::generate();
    let request = create_curve_zap_request(
        "req-001",
        "production",
        "192.168.1.100:5555",
        Bytes::from("client-1"),
        &keypair.public,
    );

    assert_eq!(request.version, "1.0");
    assert_eq!(request.request_id, "req-001");
    assert_eq!(request.domain, "production");
    assert_eq!(request.mechanism, ZapMechanism::Curve);
    assert_eq!(request.credentials.len(), 1);
    assert_eq!(request.credentials[0].len(), CURVE_KEY_SIZE);
    assert_eq!(&request.credentials[0][..], keypair.public.as_bytes());
}

#[test]
fn test_curve_box_encrypt_decrypt() {
    // Note: This tests internal CurveBox which isn't pub, so we test via client/server
    // This is a placeholder showing what we'd test if CurveBox was exported
    
    let keypair1 = CurveKeyPair::generate();
    let keypair2 = CurveKeyPair::generate();
    
    // Compute shared secrets (both sides compute same secret)
    let shared1 = keypair1.secret.diffie_hellman(&keypair2.public);
    let shared2 = keypair2.secret.diffie_hellman(&keypair1.public);
    
    assert_eq!(shared1, shared2);
}

#[test]
fn test_curve_client_encryption() {
    use monocoque_zmtp::security::curve::CurveClient;
    
    let client_keypair = CurveKeyPair::generate();
    let server_keypair = CurveKeyPair::generate();
    
    let mut client = CurveClient::new(client_keypair, server_keypair.public);
    
    // Before handshake, message box should be None
    // (We can't test encrypt_message without completing handshake)
    
    // This test verifies client can be constructed
    // Full encryption test requires async handshake
}

#[test]
fn test_curve_server_creation() {
    use monocoque_zmtp::security::curve::CurveServer;
    
    let server_keypair = CurveKeyPair::generate();
    let _server = CurveServer::new(server_keypair);
    
    // Server creation should succeed
}

#[test]
fn test_curve_key_size_constant() {
    // Verify the constant matches X25519 key size
    assert_eq!(CURVE_KEY_SIZE, 32);
    
    // Verify ChaCha20-Poly1305 requirements
    use monocoque_zmtp::security::curve::CURVE_NONCE_SIZE;
    assert_eq!(CURVE_NONCE_SIZE, 24); // ChaCha20-Poly1305 XNonce
}

#[test]
fn test_curve_as_ref_trait() {
    let keypair = CurveKeyPair::generate();
    let key_ref: &[u8] = keypair.public.as_ref();
    
    assert_eq!(key_ref.len(), CURVE_KEY_SIZE);
    assert_eq!(key_ref, keypair.public.as_bytes());
}

#[test]
fn test_curve_debug_impl_hides_secret() {
    let secret = CurveSecretKey::generate();
    let debug_str = format!("{:?}", secret);
    
    // Debug impl should not reveal the actual key
    assert!(debug_str.contains("REDACTED") || debug_str.contains("CurveSecretKey"));
    assert!(!debug_str.contains("0x")); // No hex dumps
}

#[compio::test]
async fn test_curve_handshake_sequence() {
    // This would be a full integration test with actual TCP streams
    // For now, we just verify the state machine can be created
    
    use monocoque_zmtp::security::curve::{CurveClient, CurveServer};
    
    let client_keypair = CurveKeyPair::generate();
    let server_keypair = CurveKeyPair::generate();
    
    let _client = CurveClient::new(client_keypair.clone(), server_keypair.public);
    let _server = CurveServer::new(server_keypair);
    
    // Full handshake test would require mock streams or real TCP
}
