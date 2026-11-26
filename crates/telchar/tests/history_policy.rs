//! Tests explicit terminal history retention policy.

use telchar::service::config::ServiceConfig;

fn load(contents: &str) -> std::io::Result<ServiceConfig> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.toml");
    std::fs::write(&path, contents).unwrap();
    ServiceConfig::load_with_environment(&path, |_| None)
}

#[test]
fn history_policy_is_disabled_until_explicitly_configured() {
    assert!(load("").unwrap().history_retention().is_none());
    let config = load("[history_retention]\nretention_days = 7\nmaximum_builds = 8\ndestination = '/var/lib/telchar/history'\n").unwrap();
    let policy = config.history_retention().unwrap();
    assert_eq!(policy.retention.as_secs(), 7 * 86400);
    assert_eq!(policy.maximum_builds, 8);
    assert!(policy.destination.is_some());
    assert!(
        load("[history_retention]\nretention_days = 7\ndiscard = true\n")
            .unwrap()
            .history_retention()
            .is_some()
    );
    for contents in [
        "retention_days = 0\ndiscard = true",
        "retention_days = 7",
        "retention_days = 7\ndiscard = true\ndestination = '/tmp/history'",
        "retention_days = 7\ndestination = 'relative'",
        "retention_days = 7\ndiscard = true\nmaximum_builds = 257",
    ] {
        assert!(
            load(&format!("[history_retention]\n{contents}\n")).is_err(),
            "{contents}"
        );
    }
}
