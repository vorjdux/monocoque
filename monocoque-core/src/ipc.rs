//! IPC transport via Unix domain sockets.
//!
//! Provides Unix domain socket support for inter-process communication
//! with zero-copy and low-latency characteristics.

#[cfg(unix)]
use crate::rt::{UnixListener, UnixStream};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};

/// A bound Unix domain socket listener that unlinks its socket file on drop.
///
/// A `UnixListener` does not remove its filesystem node when closed, so every
/// IPC endpoint would otherwise leave a stale socket behind (libzmq unlinks the
/// endpoint on close). This wrapper records the bound path and removes it on
/// [`Drop`], guarding the unlink so it only ever removes a socket node.
///
/// It [`Deref`]s to the underlying [`UnixListener`], so it can be used anywhere
/// a `&UnixListener` is expected (e.g. [`accept`]).
#[cfg(unix)]
#[derive(Debug)]
pub struct IpcListener {
    listener: UnixListener,
    path: PathBuf,
}

#[cfg(unix)]
impl IpcListener {
    /// Borrow the underlying listener.
    #[must_use]
    pub const fn listener(&self) -> &UnixListener {
        &self.listener
    }

    /// The filesystem path this listener is bound to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
impl std::ops::Deref for IpcListener {
    type Target = UnixListener;

    fn deref(&self) -> &Self::Target {
        &self.listener
    }
}

#[cfg(unix)]
impl Drop for IpcListener {
    fn drop(&mut self) {
        // Only remove the node if it is still the socket we bound. This avoids
        // deleting a regular file that raced into the path, and is a no-op if
        // the socket was already unlinked.
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if metadata.file_type().is_socket() {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

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
pub async fn bind<P: AsRef<Path>>(path: P) -> std::io::Result<IpcListener> {
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

    let listener = UnixListener::bind(path_ref).await?;
    Ok(IpcListener {
        listener,
        path: path_ref.to_path_buf(),
    })
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

    #[test]
    fn drop_unlinks_socket_file() {
        crate::rt::LocalRuntime::new()
            .unwrap()
            .block_on(drop_unlinks_socket_file_impl())
    }

    async fn drop_unlinks_socket_file_impl() {
        let path = std::env::temp_dir().join(format!(
            "monocoque-ipc-unlink-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let listener = bind(&path).await.unwrap();
        assert!(path.exists(), "bind should create the socket node");

        drop(listener);
        assert!(
            !path.exists(),
            "dropping IpcListener must unlink the socket file"
        );
    }

    #[test]
    fn drop_leaves_non_socket_at_path_untouched() {
        crate::rt::LocalRuntime::new()
            .unwrap()
            .block_on(drop_leaves_non_socket_at_path_untouched_impl())
    }

    async fn drop_leaves_non_socket_at_path_untouched_impl() {
        // Bind, then race a regular file into the same path before drop. Drop
        // must not delete a node that is no longer our socket.
        let path = std::env::temp_dir().join(format!(
            "monocoque-ipc-guard-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let listener = bind(&path).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"not a socket").unwrap();

        drop(listener);
        assert!(
            path.exists(),
            "Drop must not remove a non-socket node at the bound path"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bind_does_not_unlink_existing_regular_file() {
        crate::rt::LocalRuntime::new()
            .unwrap()
            .block_on(bind_does_not_unlink_existing_regular_file_impl())
    }

    async fn bind_does_not_unlink_existing_regular_file_impl() {
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
