//! Tests lifecycle detail retention policy configuration.

use super::*;
use std::time::Duration;

#[test]
fn lifecycle_detail_retention_accepts_bounded_policy_and_rejects_reload() {
    let root = fixture_root("history-retention");
    let path = root.join("telchar.toml");
    let baseline = ServiceConfig::load_with_environment(&path, |_| None).unwrap();
    assert_eq!(
        baseline.lifecycle_detail_retention(),
        Duration::from_secs(30 * 86400)
    );
    fs::write(&path, "lifecycle_detail_retention_days = 60\n").unwrap();
    let configured =
        ServiceConfig::load_with_environment(&path, |_| None).expect("retention policy loads");
    assert_eq!(
        configured.lifecycle_detail_retention(),
        Duration::from_secs(60 * 86400)
    );
    assert!(baseline.validate_static_ssh_reload(&configured).is_err());
    for days in [0, 3651] {
        fs::write(&path, format!("lifecycle_detail_retention_days = {days}\n")).unwrap();
        assert!(ServiceConfig::load_with_environment(&path, |_| None).is_err());
    }
    fs::remove_dir_all(root).unwrap();
}
