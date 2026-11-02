use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

pub(super) fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        && host.parse::<std::net::IpAddr>().is_err()
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
    ) -> io::Result<SocketAddr> {
        let timed_out =
            || io::Error::new(io::ErrorKind::TimedOut, "MicroVM callback DNS timed out");
        let mut active = self
            .active
            .lock()
            .map_err(|_| io::Error::other("callback resolver unavailable"))?;
        while *active {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(timed_out)?;
            active = self
                .available
                .wait_timeout(active, remaining)
                .map_err(|_| io::Error::other("callback resolver unavailable"))?
                .0;
        }
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        *active = true;
        let permit = Lookup(Arc::clone(self));
        drop(active);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        // A timed-out system resolver may keep running; its permit bounds outstanding work.
        std::thread::Builder::new()
            .name("microvm-dns".into())
            .spawn(move || {
                let _permit = permit;
                let result = lookup().and_then(|addresses| {
                    addresses
                        .into_iter()
                        .filter(|address| {
                            address.is_ipv4()
                                && address.port() != 0
                                && !address.ip().is_unspecified()
                                && !address.ip().is_multicast()
                        })
                        .min()
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::AddrNotAvailable,
                                "MicroVM callback DNS returned no usable IPv4 address",
                            )
                        })
                });
                let _ = sender.send(result);
            })?;
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| timed_out())?
    }
}

pub(super) fn resolve(host: &str, port: u16, deadline: Instant) -> io::Result<SocketAddr> {
    static RESOLVER: OnceLock<Arc<Resolver>> = OnceLock::new();
    let host = host.to_owned();
    RESOLVER
        .get_or_init(|| Arc::new(Resolver::default()))
        .lookup(deadline, move || {
            (host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.collect())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn lookup_refreshes_answers_and_rejects_empty_or_failed_resolution() {
        let resolver = Arc::new(Resolver::default());
        for ip in ["192.0.2.1:443", "192.0.2.2:443"] {
            let expected = ip.parse().unwrap();
            assert_eq!(
                resolver
                    .lookup(Instant::now() + Duration::from_secs(1), move || Ok(vec![
                        expected
                    ]))
                    .unwrap(),
                expected
            );
        }
        assert_eq!(
            resolver
                .lookup(Instant::now() + Duration::from_secs(1), || Ok(vec![]))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AddrNotAvailable
        );
        assert!(
            resolver
                .lookup(Instant::now() + Duration::from_secs(1), || Err(
                    io::Error::other("DNS unavailable")
                ))
                .is_err()
        );
    }

    #[test]
    fn timeout_bounds_callers_without_accumulating_lookups() {
        let resolver = Arc::new(Resolver::default());
        let (release, waiting) = std::sync::mpsc::channel();
        assert_eq!(
            resolver
                .lookup(Instant::now() + Duration::from_millis(30), move || {
                    waiting.recv().unwrap();
                    Ok(vec!["192.0.2.1:443".parse().unwrap()])
                })
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::clone(&started);
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
        assert!(!started.load(std::sync::atomic::Ordering::SeqCst));
        release.send(()).unwrap();
        assert_eq!(
            resolver
                .lookup(Instant::now() + Duration::from_secs(1), || Ok(vec![
                    "192.0.2.2:443".parse().unwrap()
                ]))
                .unwrap(),
            "192.0.2.2:443".parse::<SocketAddr>().unwrap()
        );
    }
}
