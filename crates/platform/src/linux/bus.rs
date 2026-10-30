//! D-Bus connections whose socket is opened here.
//!
//! zbus opens the socket for an address on the `blocking` crate's thread
//! pool, and that pool's idle thread then wakes every 500 ms for the life of
//! the process. Connecting synchronously and handing zbus the connected
//! stream keeps an idle process asleep.

use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};

use zbus::Address;
use zbus::address::transport::{Transport, UnixSocket};
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;

/// The session bus.
pub(crate) fn session() -> zbus::Result<Connection> {
    connect(&Address::session()?)
}

/// The bus at `address`, which must be a Unix socket, as session and
/// accessibility buses are.
pub(crate) fn connect(address: &Address) -> zbus::Result<Connection> {
    let Transport::Unix(unix) = address.transport() else {
        return Err(zbus::Error::Unsupported);
    };
    let stream = match unix.path() {
        UnixSocket::File(path) => UnixStream::connect(path)?,
        UnixSocket::Abstract(name) => {
            UnixStream::connect_addr(&SocketAddr::from_abstract_name(name.as_encoded_bytes())?)?
        }
        _ => return Err(zbus::Error::Unsupported),
    };
    Builder::async_io_unix_stream(stream).build()
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Stdio};

    use zbus::interface;

    use super::*;

    /// Answers `Hello` like a message bus, so a bus connection can complete.
    struct Daemon;

    #[interface(name = "org.freedesktop.DBus")]
    impl Daemon {
        fn hello(&self) -> String {
            ":1.7".into()
        }
    }

    fn blocking_threads() -> Vec<String> {
        std::fs::read_dir("/proc/self/task")
            .expect("tasks")
            .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
            .map(|name| name.trim().to_owned())
            .filter(|name| name.starts_with("blocking-"))
            .collect()
    }

    /// Where the child process's stand-in bus listens.
    const BUS_ENV: &str = "GHOSTCARET_TEST_BUS";

    /// Not a test on its own: the stand-in bus that the test below runs in
    /// a child process. zbus's server side reads the client's credentials on
    /// the thread pool, which would otherwise count against the client.
    #[test]
    #[ignore = "run by connecting_starts_no_pool_thread"]
    fn stand_in_bus() {
        let Some(path) = std::env::var_os(BUS_ENV) else {
            return;
        };
        let listener = UnixListener::bind(path).expect("listen");
        let (stream, _) = listener.accept().expect("accept");
        let _daemon = Builder::async_io_unix_stream(stream)
            .server(zbus::Guid::generate())
            .expect("server")
            .p2p()
            .serve_at("/org/freedesktop/DBus", Daemon)
            .expect("serve")
            .build()
            .expect("daemon connection");
        // Serves until the parent closes stdin.
        let _ = std::io::stdin().read(&mut [0]);
    }

    /// Regression: the theme watch connected with `Connection::session()`,
    /// which started a `blocking` pool thread that woke twice a second while
    /// everything else slept.
    #[test]
    fn connecting_starts_no_pool_thread() {
        let dir = std::env::temp_dir().join(format!("ghostcaret-bus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("bus");
        let _ = std::fs::remove_file(&path);
        let mut daemon = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "linux::bus::tests::stand_in_bus",
                "--ignored",
                "--quiet",
            ])
            .env(BUS_ENV, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("stand-in bus");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the stand-in bus did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let address: Address = format!("unix:path={}", path.display())
            .parse()
            .expect("address");
        let conn = connect(&address).expect("connect");
        assert_eq!(conn.unique_name().map(|n| n.as_str()), Some(":1.7"));
        let threads = blocking_threads();
        drop(daemon.stdin.take());
        let _ = daemon.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(threads.is_empty(), "{threads:?}");
    }
}
