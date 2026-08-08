use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use telchar::disk_reserve::{
    DiskReserve, DiskReserveProbe, Filesystem, ProbeError, RejectionReason,
    DEFAULT_GATEWAY_DISK_RESERVE_BYTES,
};

struct ControlledProbe {
    store: Result<Filesystem, ProbeError>,
    staging: Result<Filesystem, ProbeError>,
}

struct CountingProbe {
    filesystem: Filesystem,
    paths: Arc<Mutex<Vec<PathBuf>>>,
}

impl DiskReserveProbe for ControlledProbe {
    fn probe(&self, path: &Path) -> Result<Filesystem, ProbeError> {
        if path == Path::new("/nix/store") {
            self.store
        } else {
            self.staging
        }
    }
}

impl DiskReserveProbe for CountingProbe {
    fn probe(&self, path: &Path) -> Result<Filesystem, ProbeError> {
        self.paths
            .lock()
            .expect("probe paths available")
            .push(path.to_path_buf());
        Ok(self.filesystem)
    }
}

fn filesystem(identity: u64, available_bytes: u64) -> Result<Filesystem, ProbeError> {
    Ok(Filesystem::new(identity, available_bytes))
}

#[test]
fn parses_default_and_only_positive_u64_values() {
    assert_eq!(
        DiskReserve::default().bytes(),
        DEFAULT_GATEWAY_DISK_RESERVE_BYTES
    );
    assert_eq!(
        DiskReserve::parse("42")
            .expect("positive value parses")
            .bytes(),
        42
    );
    for value in ["0", "invalid", "-1", "18446744073709551616"] {
        assert!(DiskReserve::parse(value).is_err(), "{value} rejects");
    }
}

#[test]
fn build_admits_at_reserve_boundary() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let probe = ControlledProbe {
        store: filesystem(1, 10),
        staging: filesystem(2, 0),
    };

    reserve
        .admit_build(&probe, Path::new("/nix/store"))
        .expect("available bytes equal to reserve admit build");
}

#[test]
fn build_rejects_one_byte_below_reserve() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let probe = ControlledProbe {
        store: filesystem(1, 9),
        staging: filesystem(2, 0),
    };

    let rejection = reserve
        .admit_build(&probe, Path::new("/nix/store"))
        .expect_err("one byte below reserve rejects");
    assert_eq!(rejection.reason(), RejectionReason::InsufficientSpace);
    assert_eq!(rejection.filesystem(), "gateway-store");
    assert_eq!(rejection.required_bytes(), 10);
    assert_eq!(rejection.available_bytes(), Some(9));
}

#[test]
fn transfer_admits_at_different_filesystem_boundary() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let probe = ControlledProbe {
        store: filesystem(1, 13),
        staging: filesystem(2, 13),
    };

    reserve
        .admit_transfer(&probe, Path::new("/nix/store"), Path::new("/staging"), 3)
        .expect("both filesystems have reserve plus NAR size");
}

#[test]
fn transfer_admits_at_shared_filesystem_boundary() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let probe = ControlledProbe {
        store: filesystem(1, 16),
        staging: filesystem(1, 16),
    };

    reserve
        .admit_transfer(&probe, Path::new("/nix/store"), Path::new("/staging"), 3)
        .expect("shared filesystem has reserve plus two NAR copies");
}

#[test]
fn shared_filesystem_uses_lower_observed_availability() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let probe = ControlledProbe {
        store: filesystem(1, 16),
        staging: filesystem(1, 15),
    };

    let rejection = reserve
        .admit_transfer(&probe, Path::new("/nix/store"), Path::new("/staging"), 3)
        .expect_err("lower shared-filesystem observation rejects");

    assert_eq!(rejection.reason(), RejectionReason::InsufficientSpace);
    assert_eq!(rejection.filesystem(), "shared");
    assert_eq!(rejection.required_bytes(), 16);
    assert_eq!(rejection.available_bytes(), Some(15));
}

#[test]
fn transfer_rejects_one_byte_below_requirements() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let different = ControlledProbe {
        store: filesystem(1, 13),
        staging: filesystem(2, 12),
    };
    let shared = ControlledProbe {
        store: filesystem(1, 15),
        staging: filesystem(1, 15),
    };

    let different_rejection = reserve
        .admit_transfer(
            &different,
            Path::new("/nix/store"),
            Path::new("/staging"),
            3,
        )
        .expect_err("one byte below staging requirement rejects");
    assert_eq!(
        different_rejection.reason(),
        RejectionReason::InsufficientSpace
    );
    assert_eq!(different_rejection.filesystem(), "staging");
    let shared_rejection = reserve
        .admit_transfer(&shared, Path::new("/nix/store"), Path::new("/staging"), 3)
        .expect_err("one byte below shared requirement rejects");
    assert_eq!(
        shared_rejection.reason(),
        RejectionReason::InsufficientSpace
    );
    assert_eq!(shared_rejection.filesystem(), "shared");
}

#[test]
fn transfer_treats_different_path_text_on_one_filesystem_as_shared() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let paths = Arc::new(Mutex::new(Vec::new()));
    let probe = CountingProbe {
        filesystem: Filesystem::new(7, 16),
        paths: Arc::clone(&paths),
    };

    reserve
        .admit_transfer(
            &probe,
            Path::new("/nix/store"),
            Path::new("/private/staging"),
            3,
        )
        .expect("one filesystem uses shared reserve calculation");

    assert_eq!(
        *paths.lock().expect("probe paths available"),
        vec![
            PathBuf::from("/nix/store"),
            PathBuf::from("/private/staging")
        ],
        "policy probes each actual measured directory"
    );
}

#[test]
fn transfer_rejects_different_filesystem_addition_overflow() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let probe = ControlledProbe {
        store: filesystem(1, u64::MAX),
        staging: filesystem(2, u64::MAX),
    };

    let rejection = reserve
        .admit_transfer(
            &probe,
            Path::new("/nix/store"),
            Path::new("/staging"),
            u64::MAX,
        )
        .expect_err("overflow rejects");
    assert_eq!(rejection.reason(), RejectionReason::ArithmeticOverflow);
    assert_eq!(rejection.filesystem(), "gateway-store");
}

#[test]
fn transfer_rejects_arithmetic_overflow() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let probe = ControlledProbe {
        store: filesystem(1, u64::MAX),
        staging: filesystem(1, u64::MAX),
    };

    let rejection = reserve
        .admit_transfer(
            &probe,
            Path::new("/nix/store"),
            Path::new("/staging"),
            u64::MAX,
        )
        .expect_err("overflow rejects");
    assert_eq!(rejection.reason(), RejectionReason::ArithmeticOverflow);
    assert_eq!(rejection.filesystem(), "shared");
}

#[test]
fn transfer_propagates_probe_failure() {
    let reserve = DiskReserve::parse("10").expect("positive reserve parses");
    let probe = ControlledProbe {
        store: Err(ProbeError::Failed),
        staging: filesystem(1, u64::MAX),
    };

    let rejection = reserve
        .admit_transfer(&probe, Path::new("/nix/store"), Path::new("/staging"), 1)
        .expect_err("probe failure rejects");
    assert_eq!(rejection.reason(), RejectionReason::ProbeFailed);
    assert_eq!(rejection.filesystem(), "gateway-store");
}
