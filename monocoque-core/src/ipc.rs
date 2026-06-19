//! IPC transport via Unix domain sockets.
//!
//! Provides Unix domain socket support for inter-process communication
//! with zero-copy and low-latency characteristics.

#[cfg(unix)]
use crate::rt::{UnixListener, UnixStream};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
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
/// # async fn example() -> std::io::Result<()> {
/// let stream = ipc::connect("/tmp/socket.sock").await?;
/// # Ok(())
/// # }
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
/// # async fn example() -> std::io::Result<()> {
/// let listener = ipc::bind("/tmp/socket.sock").await?;
/// # Ok(())
/// # }
/// ```
pub async fn bind<P: AsRef<Path>>(path: P) -> std::io::Result<UnixListener> {
    let path_ref = path.as_ref();
    if path_ref.exists() {
        let metadata = std::fs::symlink_metadata(path_ref)?;
        if metadata.file_type().is_socket() {
            std::fs::remove_file(path_ref)?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "IPC bind path exists and is not a Unix socket",
            ));
        }
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

    #[test]
    fn test_ipc_connect_bind() {
        crate::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_ipc_connect_bind_impl())
    }

    async fn test_ipc_connect_bind_impl() {
        let path = "/tmp/monocoque_test_ipc.sock";

        // Clean up any existing socket
        let _ = std::fs::remove_file(path);

        let listener = bind(path).await.unwrap();

        // Spawn accept task
        let accept_handle = crate::rt::spawn(async move { accept(&listener).await });

        // Give listener time to start
        crate::rt::sleep(std::time::Duration::from_millis(10)).await;

        // Connect
        let client = connect(path).await.unwrap();

        // Wait for accept
        let server = crate::rt::join(accept_handle).await.unwrap();

        assert!(client.peer_addr().is_ok());
        assert!(server.local_addr().is_ok());

        // Cleanup
        drop(client);
        drop(server);
        let _ = std::fs::remove_file(path);
    }

    #[compio::test]
    async fn bind_does_not_unlink_existing_regular_file() {
        let path = std::env::temp_dir().join(format!(
            "monocoque-ipc-regular-file-{}.sock",
            std::process::id()
        ));

        std::fs::write(&path, b"do not delete").unwrap();

        let result = bind(&path).await;

        assert!(
            result.is_err(),
            "binding over an existing regular file should fail instead of unlinking it"
        );
        assert!(
            path.exists(),
            "ipc::bind unlinked an existing regular file before binding"
        );

        let _ = std::fs::remove_file(path);
    }
}
