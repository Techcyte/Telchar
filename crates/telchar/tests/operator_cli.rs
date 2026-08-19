//! Tests the bounded read-only operator command surface.

mod support;

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use support::postgres::PostgresFixture;

#[test]
fn config_check_reports_configured_backends_without_database_access() {
    let root = fixture_root("config");
    let config = root.join("telchar.toml");
    fs::write(
        &config,
        "[backends.local]\nname = \"local-main\"\nsystem = \"x86_64-linux\"\nsupported_features = [\"kvm\"]\nmaximum_concurrent_builds = 2\n",
    )
    .expect("configuration writes");

    let output = operator(&config, &["config-check"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let report = json(&output);
    assert_eq!(report["valid"], true);
    assert_eq!(report["backend_count"], 1);
    assert_eq!(report["backends"][0]["name"], "local-main");
    assert_eq!(report["backends"][0]["kind"], "local");
    assert_eq!(report["backends"][0]["system"], "x86_64-linux");
    assert_eq!(
        report["backends"][0]["features"],
        serde_json::json!(["kvm"])
    );
    assert_eq!(report["backends"][0]["capacity"], 2);
}

#[test]
fn durable_commands_report_empty_authoritative_state() {
    let database = PostgresFixture::start();
    telchar::persistence::migrate(database.url()).expect("database migrates");
    let root = fixture_root("durable");
    let config = root.join("telchar.toml");
    let database_url_file = root.join("database-url");
    fs::write(&database_url_file, format!("{}\n", database.url())).expect("database URL writes");
    fs::write(
        &config,
        format!(
            "[database]\nurl_file = \"{}\"\n\n[backends.local]\nname = \"local-main\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 2\n",
            database_url_file.display()
        ),
    )
    .expect("configuration writes");

    let derivation = "/nix/store/11111111111111111111111111111111-operator.drv";
    telchar::persistence::claim_shared_build(
        database.url(),
        derivation,
        &[7; 32],
        "local-main",
        telchar::backend::BackendKind::Local,
        telchar::backend::BackendKind::Local.capabilities(),
        None,
        &["/nix/store/22222222222222222222222222222222-operator"],
    )
    .expect("shared build claims");
    telchar::persistence::enqueue_shared_build(database.url(), derivation, "operator-subject", 10)
        .expect("shared build queues");

    let queue = operator(&config, &["queue", "--limit", "10"]);
    assert!(queue.status.success(), "{}", stderr(&queue));
    let report = json(&queue);
    assert_eq!(report["builds"][0]["derivation_path"], derivation);
    assert_eq!(report["builds"][0]["queue_position"], 1);

    telchar::persistence::start_queued_shared_build(database.url(), derivation, 10)
        .expect("shared build starts");

    let status = operator(&config, &["status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    assert_eq!(
        json(&status),
        serde_json::json!({
            "queued": 0,
            "running": 1,
            "collecting": 0,
            "active": 1
        })
    );

    let build = operator(&config, &["build", derivation]);
    assert!(build.status.success(), "{}", stderr(&build));
    let report = json(&build);
    assert_eq!(report["derivation_path"], derivation);
    assert_eq!(report["state"], "running");
    assert_eq!(report["backend_name"], "local-main");

    let backends = operator(&config, &["backends"]);
    assert!(backends.status.success(), "{}", stderr(&backends));
    let report = json(&backends);
    assert_eq!(report["backends"][0]["name"], "local-main");
    assert_eq!(report["backends"][0]["capacity"], 2);
    assert_eq!(report["backends"][0]["active_builds"], 1);
    assert!(report["backends"][0].get("available").is_none());

    let recovery = operator(&config, &["recovery", "--limit", "10"]);
    assert!(recovery.status.success(), "{}", stderr(&recovery));
    let report = json(&recovery);
    assert_eq!(report["builds"][0]["derivation_path"], derivation);
    assert_eq!(report["builds"][0]["recovery"], "output-only");
}

#[test]
fn operator_rejects_mutating_and_unbounded_requests() {
    let root = fixture_root("arguments");
    let config = root.join("telchar.toml");
    fs::write(
        &config,
        "[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 1\n",
    )
    .expect("configuration writes");

    for arguments in [
        vec!["cancel"],
        vec!["queue", "--limit", "0"],
        vec!["queue", "--limit", "257"],
        vec!["recovery", "--limit", "not-a-number"],
    ] {
        let output = operator(&config, &arguments);
        assert!(!output.status.success(), "{arguments:?}");
    }
}

fn operator(config: &std::path::Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_telchar"))
        .arg("operator")
        .args(arguments)
        .env("TELCHAR_CONFIG", config)
        .output()
        .expect("operator command starts")
}

fn json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("operator output is JSON")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn fixture_root(name: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("telchar-operator-{name}-{nonce}"));
    fs::create_dir(&root).expect("fixture creates");
    root
}
