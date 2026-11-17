//! CURVE encryption with client authorization.
//!
//! CURVE (RFC 26) gives two things by itself:
//!
//! - every message is encrypted and authenticated (`crypto_box`), with fresh
//!   keys for each connection;
//! - the client knows it is talking to the server whose public key it was
//!   given.
//!
//! It does not decide which clients may connect. The server learns each
//! client's public key during the handshake, and a ZAP handler has to check
//! it. This example runs one with an allowlist of client keys, so a client
//! with any other key is refused before it can send a message.
//!
//! ## Running
//!
//! Both ends in one process, with fresh keys:
//! ```bash
//! cargo run --example curve_demo --features zmq demo
//! ```
//!
//! Or as two programs. Generate a key pair for each side:
//! ```bash
//! cargo run --example curve_demo --features zmq keygen   # the server's
//! cargo run --example curve_demo --features zmq keygen   # the client's
//! ```
//! then, in two terminals:
//! ```bash
//! cargo run --example curve_demo --features zmq server <server_secret> <client_public>
//! cargo run --example curve_demo --features zmq client <server_public> <client_secret>
//! ```

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::security::curve::{CURVE_KEY_SIZE, CurveKeyPair, CurveSecretKey};
use monocoque::zmq::security::plain::StaticPlainHandler;
use monocoque::zmq::security::zap_handler::{DefaultZapHandler, spawn_zap_server};
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use std::sync::Arc;
use std::time::Duration;

type Key = [u8; CURVE_KEY_SIZE];
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ENDPOINT: &str = "tcp://127.0.0.1:5556";
const REQUESTS: usize = 3;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str);

    match (arg(1), arg(2), arg(3)) {
        (Some("keygen"), None, None) => {
            let pair = CurveKeyPair::generate();
            println!("public: {}", hex::encode(pair.public.as_bytes()));
            println!(
                "secret: {}  (keep this one private)",
                hex::encode(pair.secret.to_bytes())
            );
        }
        (Some("server"), Some(server_secret), Some(client_public)) => {
            let (mut socket, _) = bind_server(
                parse_key(server_secret)?,
                parse_key(client_public)?,
                ENDPOINT,
            )
            .await?;
            serve(&mut socket, usize::MAX).await?;
        }
        (Some("client"), Some(server_public), Some(client_secret)) => {
            run_client(
                ENDPOINT,
                parse_key(server_public)?,
                parse_key(client_secret)?,
            )
            .await?;
        }
        (Some("demo"), None, None) => {
            let server = CurveKeyPair::generate();
            let client = CurveKeyPair::generate();
            // Port 0: the OS picks a free port and `bind` reports it.
            let (mut socket, endpoint) = bind_server(
                server.secret.to_bytes(),
                *client.public.as_bytes(),
                "tcp://127.0.0.1:0",
            )
            .await?;
            let serving = rt::spawn(async move {
                let served = serve(&mut socket, REQUESTS).await;
                socket.close().await?;
                served
            });
            run_client(
                &endpoint,
                *server.public.as_bytes(),
                client.secret.to_bytes(),
            )
            .await?;
            rt::join(serving).await?;
        }
        _ => {
            let me = arg(0).unwrap_or("curve_demo");
            eprintln!("Usage:");
            eprintln!("  {me} demo");
            eprintln!("  {me} keygen");
            eprintln!("  {me} server <server_secret_hex> <client_public_hex>");
            eprintln!("  {me} client <server_public_hex> <client_secret_hex>");
            std::process::exit(2);
        }
    }
    Ok(())
}

fn parse_key(hex_key: &str) -> Result<Key> {
    let bytes = hex::decode(hex_key)?;
    Key::try_from(bytes.as_slice())
        .map_err(|_| format!("a key is {CURVE_KEY_SIZE} bytes, got {}", bytes.len()).into())
}

/// Start the ZAP handler, then bind a CURVE server that admits only the
/// client whose public key is `allowed_client`. Returns the socket and the
/// endpoint it is bound to.
async fn bind_server(
    server_secret: Key,
    allowed_client: Key,
    endpoint: &str,
) -> Result<(RepSocket, String)> {
    // The handler has to be running before the first client arrives. It is
    // asked about every CURVE client of every server socket in this process.
    // `true` lets it accept CURVE at all; the whitelist names who.
    let zap = DefaultZapHandler::new(Arc::new(StaticPlainHandler::new()), true)
        .with_curve_whitelist(vec![allowed_client]);
    spawn_zap_server(Arc::new(zap))?;

    // The public key is derived from the secret one.
    let server_public = *CurveSecretKey::from_bytes(server_secret)
        .public_key()
        .as_bytes();
    let options = SocketOptions::default()
        .with_curve_server(true)
        .with_curve_keypair(server_public, server_secret)
        // With a domain set, clients are refused if the handler is ever not
        // running, instead of being let in unchecked.
        .with_zap_domain("curve_demo");

    let mut socket = RepSocket::with_options(options);
    let bound = socket.bind(endpoint).await?;
    println!("server: listening on {bound}");
    println!("server: public key {}", hex::encode(server_public));
    Ok((socket, bound))
}

async fn serve(socket: &mut RepSocket, requests: usize) -> Result<()> {
    for _ in 0..requests {
        // The borrowed message is only read, so it is turned into the reply
        // before the next call on the socket.
        let reply = {
            let request = socket.recv().await?;
            let text = String::from_utf8_lossy(request.first().unwrap_or_default());
            println!("server: got \"{text}\"");
            format!("reply to: {text}")
        };
        socket.send(reply).await?;
    }
    Ok(())
}

async fn run_client(endpoint: &str, server_public: Key, client_secret: Key) -> Result<()> {
    let client_public = *CurveSecretKey::from_bytes(client_secret)
        .public_key()
        .as_bytes();
    let options = SocketOptions::default()
        .with_curve_keypair(client_public, client_secret)
        // Pinning the server's key is what rules out a man in the middle.
        .with_curve_serverkey(server_public)
        .with_recv_timeout(Duration::from_secs(5))
        .with_send_timeout(Duration::from_secs(5));

    // The handshake runs as soon as the connection is up. A client whose key
    // is not on the server's list never gets a reply: its receive times out.
    let mut socket = ReqSocket::with_options(options);
    socket.connect(endpoint).await?;

    for i in 1..=REQUESTS {
        socket.send(format!("secure message #{i}")).await?;
        let reply = socket.recv().await?;
        let text = String::from_utf8_lossy(reply.first().unwrap_or_default());
        println!("client: got \"{text}\"");
    }
    socket.close().await?;
    Ok(())
}
