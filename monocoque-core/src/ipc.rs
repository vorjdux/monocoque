//! IPC transport via Unix domain sockets.
//!
//! Provides Unix domain socket support for inter-process communication
//! with zero-copy and low-latency characteristics.

#[cfg(unix)]
use compio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
/// Connect to a Unix domain socket.
///
/// # Examples
///
/// ```no_run
/// use monocoque_core::ipc;
///
/// #[compio::main]
/// async fn main() -> std::io::Result<()> {
///     let stream = ipc::connect("/tmp/socket.sock").await?;
///     Ok(())
/// }
/// ```
pub async fn connect<P: AsRef<Path>>(path: P) -> std::io::Result<UnixStream> {
    UnixStream::connect(path).await
}

#[cfg(unix)]
/// Bind a Unix domain socket listener.
///
/// Returns the listener ready to accept connections.
///
/// # Examples
///
/// ```no_run
/// use monocoque_core::ipc;
///
/// #[compio::main]
/// async fn main() -> std::io::Result<()> {
///     let listener = ipc::bind("/tmp/socket.sock").await?;
///     Ok(())
/// }
/// ```
pub async fn bind<P: AsRef<Path>>(path: P) -> std::io::Result<UnixListener> {
    // Remove existing socket file if it exists
    let path_ref = path.as_ref();
    if path_ref.exists() {
        std::fs::remove_file(path_ref)?;
    }
    
    UnixListener::bind(path).await
}

#[cfg(unix)]
/// Accept a connection on a Unix domain socket listener.
pub async fn accept(listener: &UnixListener) -> std::io::Result<UnixStream> {
    let (stream, _addr) = listener.accept().await?;
    Ok(stream)
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    #[compio::test]
    async fn test_ipc_connect_bind() {
        let path = "/tmp/monocoque_test_ipc.sock";
        
        // Clean up any existing socket
        let _ = std::fs::remove_file(path);
        
        let listener = bind(path).await.unwrap();
        
        // Spawn accept task
        let accept_handle = compio::runtime::spawn(async move {
            accept(&listener).await
        });
        
        // Give listener time to start
        compio::time::sleep(std::time::Duration::from_millis(10)).await;
        
        // Connect
        let client = connect(path).await.unwrap();
        
        // Wait for accept
        let server = accept_handle.await.unwrap();
        
        assert!(client.peer_addr().is_ok());
        assert!(server.local_addr().is_ok());
        
        // Cleanup
        drop(client);
        drop(server);
        let _ = std::fs::remove_file(path);
    }
}
