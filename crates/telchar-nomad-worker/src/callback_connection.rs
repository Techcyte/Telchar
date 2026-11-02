//! Deadline-bounded setup for the blocking Nomad callback transport.

use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

use tungstenite::handshake::client::{Request, Response};
use tungstenite::http::Uri;
use tungstenite::protocol::WebSocketConfig;

pub(super) fn remaining(deadline: Instant) -> io::Result<std::time::Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(timed_out)
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "worker callback setup timed out")
}

#[derive(Default)]
struct Resolver {
    active: Mutex<bool>,
    available: Condvar,
}

struct Lookup(Arc<Resolver>);

impl Drop for Lookup {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.active.lock() {
            *active = false;
            self.0.available.notify_one();
        }
    }
}

impl Resolver {
    fn lookup(
        self: &Arc<Self>,
        deadline: Instant,
        lookup: impl FnOnce() -> io::Result<Vec<SocketAddr>> + Send + 'static,
    ) -> io::Result<Vec<SocketAddr>> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| io::Error::other("worker resolver unavailable"))?;
        while *active {
            active = self
                .available
                .wait_timeout(active, remaining(deadline)?)
                .map_err(|_| io::Error::other("worker resolver unavailable"))?
                .0;
        }
        remaining(deadline)?;
        *active = true;
        let permit = Lookup(Arc::clone(self));
        drop(active);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        // System DNS cannot be cancelled; retain the sole permit until it actually exits.
        std::thread::Builder::new()
            .name("nomad-callback-dns".into())
            .spawn(move || {
                let _permit = permit;
                let _ = sender.send(lookup());
            })?;
        receiver
            .recv_timeout(remaining(deadline)?)
            .map_err(|_| timed_out())?
    }
}

fn resolve(uri: &Uri, deadline: Instant) -> io::Result<Vec<SocketAddr>> {
    static RESOLVER: OnceLock<Arc<Resolver>> = OnceLock::new();
    tungstenite::client::uri_mode(uri)
        .map_err(|_| io::Error::other("worker callback scheme is invalid"))?;
    let host = uri
        .host()
        .ok_or_else(|| io::Error::other("worker callback host is missing"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("wss") {
            443
        } else {
            80
        });
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        remaining(deadline)?;
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let host = host.to_owned();
    RESOLVER
        .get_or_init(|| Arc::new(Resolver::default()))
        .lookup(deadline, move || {
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.collect())
        })
}

fn connect_addresses(addresses: &[SocketAddr], deadline: Instant) -> io::Result<TcpStream> {
    let mut error = io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "worker callback has no addresses",
    );
    for address in addresses {
        match TcpStream::connect_timeout(address, remaining(deadline)?) {
            Ok(stream) => return Ok(stream),
            Err(failure) => error = failure,
        }
    }
    Err(error)
}

pub(super) struct SetupDeadline {
    finished: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SetupDeadline {
    fn start(stream: &TcpStream, deadline: Instant) -> io::Result<Self> {
        let stream = stream.try_clone()?;
        let (finished, completion) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("nomad-setup-deadline".into())
            .spawn(move || {
                if completion.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            })?;
        Ok(Self {
            finished,
            thread: Some(thread),
        })
    }
}

impl Drop for SetupDeadline {
    fn drop(&mut self) {
        let _ = self.finished.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) fn connect(
    request: Request,
    config: WebSocketConfig,
    deadline: Instant,
) -> io::Result<(super::WorkerSocket, Response, SetupDeadline)> {
    let (parts, _) = request.into_parts();
    let mut uri = parts.uri.clone();
    // Every redirect hop shares the setup deadline.
    for attempt in 0..=3 {
        let addresses = resolve(&uri, deadline)?;
        let stream = connect_addresses(&addresses, deadline)?;
        stream.set_nodelay(true)?;
        let timeout = remaining(deadline)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let monitor = SetupDeadline::start(&stream, deadline)?;
        let mut request = Request::builder()
            .uri(uri.clone())
            .method(parts.method.clone())
            .version(parts.version);
        *request.headers_mut().expect("request builder has headers") = parts.headers.clone();
        let request = request
            .body(())
            .map_err(|_| io::Error::other("worker callback request is invalid"))?;
        match tungstenite::client_tls_with_config(request, stream, Some(config), None) {
            Ok((socket, response)) => {
                remaining(deadline)?;
                return Ok((socket, response, monitor));
            }
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response)))
                if response.status().is_redirection() && attempt < 3 =>
            {
                uri = response
                    .headers()
                    .get("Location")
                    .and_then(|location| location.to_str().ok())
                    .and_then(|location| location.parse().ok())
                    .ok_or_else(|| io::Error::other("worker callback redirect is invalid"))?;
            }
            Err(_) => {
                remaining(deadline)?;
                return Err(io::Error::other("worker callback connection failed"));
            }
        }
    }
    Err(io::Error::other("worker callback connection failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stalled_dns_bounds_callers_and_outstanding_resolvers() {
        let resolver = Arc::new(Resolver::default());
        let (release, blocked) = std::sync::mpsc::channel();
        assert_eq!(
            resolver
                .lookup(Instant::now() + Duration::from_millis(30), move || {
                    blocked.recv().unwrap();
                    Ok(vec![])
                })
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::clone(&called);
        for _ in 0..3 {
            let observed = Arc::clone(&observed);
            assert_eq!(
                resolver
                    .lookup(Instant::now() + Duration::from_millis(30), move || {
                        observed.store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(vec![])
                    })
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::TimedOut
            );
        }
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        release.send(()).unwrap();
        let expected = "127.0.0.1:443".parse().unwrap();
        assert_eq!(
            resolver
                .lookup(Instant::now() + Duration::from_secs(1), move || Ok(vec![
                    expected
                ]))
                .unwrap(),
            vec![expected]
        );
    }

    #[test]
    fn address_candidates_and_expired_budget() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let refused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let refused_address = refused.local_addr().unwrap();
        drop(refused);
        let addresses = [refused_address, listener.local_addr().unwrap()];
        let stream =
            connect_addresses(&addresses, Instant::now() + Duration::from_secs(1)).unwrap();
        drop(stream);
        assert_eq!(
            connect_addresses(&addresses, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
}
