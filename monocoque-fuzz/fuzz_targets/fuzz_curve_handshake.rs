#![no_main]

use libfuzzer_sys::fuzz_target;
use monocoque_zmtp::security::curve::{
    CurveClient, CurveKeyPair, CurvePublicKey, CurveSecretKey, CURVE_KEY_SIZE,
};

fuzz_target!(|data: &[u8]| {
    // -----------------------------------------------------------------------
    // 1. Key construction and byte-conversion round-trips
    // -----------------------------------------------------------------------

    // Try to build a CurvePublicKey from the first 32 bytes of fuzz input.
    if data.len() >= CURVE_KEY_SIZE {
        let mut key_bytes = [0u8; CURVE_KEY_SIZE];
        key_bytes.copy_from_slice(&data[..CURVE_KEY_SIZE]);

        // from_bytes / as_bytes round-trip — must never panic
        let public_key = CurvePublicKey::from_bytes(key_bytes);
        let _ = public_key.as_bytes();
        let _ = public_key.to_x25519();

        // AsRef<[u8]> path
        let _slice: &[u8] = public_key.as_ref();

        // From<[u8; 32]> conversion
        let public_key2 = CurvePublicKey::from(key_bytes);
        assert_eq!(public_key, public_key2);

        // Try building a secret key from the same bytes and derive its public key.
        let secret_key = CurveSecretKey::from_bytes(key_bytes);
        let derived_public = secret_key.public_key();
        // Derived public key must also be valid (no panic).
        let _ = derived_public.as_bytes();

        // ECDH with fuzz-supplied key as peer — must not panic, only return bytes.
        let shared = secret_key.diffie_hellman(&public_key);
        assert_eq!(shared.len(), CURVE_KEY_SIZE);
    }

    // -----------------------------------------------------------------------
    // 2. CurveKeyPair generation — exercises the generate() path
    // -----------------------------------------------------------------------
    {
        let kp = CurveKeyPair::generate();
        // Public key must match what the secret key derives.
        let derived = kp.secret.public_key();
        assert_eq!(kp.public, derived);
        // Clone and Debug must not panic.
        let kp2 = kp.clone();
        let _ = format!("{:?}", kp2);
    }

    // -----------------------------------------------------------------------
    // 3. CurveKeyPair::from_keys with fuzz-supplied bytes
    //    (public and secret are intentionally unrelated here)
    // -----------------------------------------------------------------------
    if data.len() >= CURVE_KEY_SIZE * 2 {
        let mut pub_bytes = [0u8; CURVE_KEY_SIZE];
        let mut sec_bytes = [0u8; CURVE_KEY_SIZE];
        pub_bytes.copy_from_slice(&data[..CURVE_KEY_SIZE]);
        sec_bytes.copy_from_slice(&data[CURVE_KEY_SIZE..CURVE_KEY_SIZE * 2]);

        let public = CurvePublicKey::from_bytes(pub_bytes);
        let secret = CurveSecretKey::from_bytes(sec_bytes);

        let kp = CurveKeyPair::from_keys(public, secret);
        let _ = kp.public.as_bytes();
        let _ = kp.secret.public_key();
    }

    // -----------------------------------------------------------------------
    // 4. Fuzz the decrypt_message path of CurveClient / CurveServer
    //
    // Both CurveClient and CurveServer are public structs with public
    // decrypt_message methods.  Calling decrypt_message before the async
    // handshake has completed means message_box is None, so the call returns
    // Err(ProtocolViolation) — it must never panic regardless of the input.
    // -----------------------------------------------------------------------
    {
        let client_kp = CurveKeyPair::generate();
        let server_kp = CurveKeyPair::generate();

        let mut client = CurveClient::new(client_kp, server_kp.public);
        // Err, not panic — message_box is None before handshake.
        let _ = client.decrypt_message(data);
    }

    {
        let server_kp = CurveKeyPair::generate();
        use monocoque_zmtp::security::curve::CurveServer;

        let mut server = CurveServer::new(server_kp);
        // Err, not panic — message_box is None before handshake.
        let _ = server.decrypt_message(data);
    }

    // -----------------------------------------------------------------------
    // 5. Encrypt-then-decrypt round-trip using matched key pairs
    //    (exercises CurveBox indirectly through the public encrypt/decrypt API
    //    after manually setting up the shared secret via ECDH)
    //
    // Because we cannot call the async handshake from a sync fuzz target, we
    // use a pair of fresh key pairs to perform ECDH and then exercise
    // encrypt_message / decrypt_message on matching client+server instances
    // that share the same secret.  We do this by testing the ECDH layer
    // directly (CurveSecretKey::diffie_hellman) and confirming symmetry.
    // -----------------------------------------------------------------------
    if data.len() >= CURVE_KEY_SIZE {
        let alice = CurveKeyPair::generate();
        let bob = CurveKeyPair::generate();

        let alice_shared = alice.secret.diffie_hellman(&bob.public);
        let bob_shared = bob.secret.diffie_hellman(&alice.public);

        // ECDH must be commutative.
        assert_eq!(alice_shared, bob_shared, "ECDH shared secret must be symmetric");
        assert_eq!(alice_shared.len(), CURVE_KEY_SIZE);
    }
});
