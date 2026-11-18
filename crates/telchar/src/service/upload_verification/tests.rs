use super::*;

fn key(name: &str) -> String {
    format!("{name}:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
}

#[test]
fn resolver_retains_snapshot_and_quarantines_detailed_duplicates() {
    let mut source = KeySource::default();
    let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"keys":[{"publicKey":key("ci"),"metadata":{"identity":"alice"}}]})).unwrap();
    source.replace(&bytes, "file").unwrap();
    assert_eq!(source.keys.len(), 1);
    assert!(
        source
            .replace(br#"{"version":2,"keys":[]}"#, "file")
            .is_err()
    );
    assert!(
        source
            .replace(br#"{"version":1,"keys":[]}"#, "file")
            .is_err()
    );
    assert_eq!(source.keys.len(), 1);
    let bytes = serde_json::to_vec(
        &serde_json::json!({"version":1,"keys":[{"publicKey":key("ci")},{"publicKey":key("ci")}]}),
    )
    .unwrap();
    source.replace(&bytes, "file").unwrap();
    assert!(source.keys.is_empty());
    assert!(source.rejected.contains("ci"));
}

#[test]
fn registry_combines_simple_with_one_detailed_but_not_two() {
    let config: UploadVerification =
        toml::from_str(&format!("trusted_public_keys = [\"{}\"]", key("ci"))).unwrap();
    let mut source = KeySource::default();
    source.replace(&serde_json::to_vec(&serde_json::json!({"version":1,"keys":[{"publicKey":key("ci"),"revoked":true,"metadata":{"identity":"alice"}}]})).unwrap(), "file").unwrap();
    let keys = merge_keys(&config.static_keys().unwrap(), &[&source]);
    assert!(keys["ci"].revoked);
    assert_eq!(keys["ci"].source, "file");
    assert!(merge_keys(&config.static_keys().unwrap(), &[&source, &source]).is_empty());
}
