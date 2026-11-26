use telchar::service::config::ServiceConfig;

fn load(contents: &str) -> std::io::Result<ServiceConfig> {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.toml");
    std::fs::write(&path, contents).unwrap();
    ServiceConfig::load_with_environment(&path, |_| None)
}

#[test]
fn log_policy_requires_explicit_bounded_configuration_and_restart() {
    let disabled = load("").unwrap();
    assert!(disabled.log_retention().is_none());
    let fields = [
        ("retention_days", "7"),
        ("directory", "'/var/lib/telchar/logs'"),
        ("maximum_bytes", "1024"),
        ("maximum_queue_bytes", "1024"),
        ("maximum_captures", "1"),
        ("maximum_spool_bytes", "67584"),
    ];
    let policy = |replacement: Option<(&str, &str)>| {
        let mut text = "[log_retention]\n".to_owned();
        for (name, value) in fields {
            let value = replacement
                .filter(|(key, _)| *key == name)
                .map_or(value, |(_, value)| value);
            if !value.is_empty() {
                text.push_str(&format!("{name} = {value}\n"));
            }
        }
        text
    };
    let enabled = load(&policy(None)).unwrap();
    assert_eq!(enabled.log_retention().unwrap().retention_days, 7);
    assert!(enabled.validate_static_ssh_reload(&enabled).is_ok());
    assert!(disabled.validate_static_ssh_reload(&enabled).is_err());
    assert!(
        enabled
            .validate_static_ssh_reload(&load(&policy(Some(("retention_days", "8")))).unwrap())
            .is_err()
    );
    for invalid in [
        ("retention_days", ""),
        ("retention_days", "0"),
        ("retention_days", "3651"),
        ("directory", "'relative'"),
        ("maximum_bytes", "0"),
        ("maximum_bytes", "8388609"),
        ("maximum_queue_bytes", "16777217"),
        ("maximum_captures", "65"),
        ("maximum_spool_bytes", "67583"),
    ] {
        assert!(load(&policy(Some(invalid))).is_err(), "{invalid:?}");
    }
}
