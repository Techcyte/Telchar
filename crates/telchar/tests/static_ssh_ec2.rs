//! EC2 membership discovery configuration contracts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use telchar::service::config::ServiceConfig;

fn configuration(root: &std::path::Path, source: &str) -> std::path::PathBuf {
    let identity = root.join("identity");
    let known_hosts = root.join("known-hosts");
    let ssh = root.join("ssh");
    fs::write(&identity, "private").unwrap();
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&known_hosts, "@cert-authority * ssh-ed25519 AAAA\n").unwrap();
    fs::write(&ssh, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let path = root.join("telchar.toml");
    fs::write(
        &path,
        format!(
            r#"
[[backends.ssh]]
system = "x86_64-linux"
maximum_concurrent_builds = 2
identity_file = "{}"
known_hosts_file = "{}"
ssh_program = "{}"

[backends.ssh.builders]
{source}
"#,
            identity.display(),
            known_hosts.display(),
            ssh.display()
        ),
    )
    .unwrap();
    path
}

#[test]
fn maps_only_running_tagged_instances_with_selected_addresses() {
    use aws_sdk_ec2::types::{Instance, InstanceState, InstanceStateName, Tag};
    let root = tempfile::tempdir().unwrap();
    let path = configuration(
        root.path(),
        r#"
source = "ec2"
region = "us-east-1"
[backends.ssh.builders.tags]
telchar-pool = "builders"
"#,
    );
    let config = ServiceConfig::load_from_default(&path).unwrap();
    let instance = Instance::builder()
        .instance_id("i-0123456789abcdef0")
        .state(
            InstanceState::builder()
                .name(InstanceStateName::Running)
                .build(),
        )
        .private_ip_address("10.0.0.1")
        .public_ip_address("203.0.113.1")
        .tags(Tag::builder().key("telchar-pool").value("builders").build())
        .tags(Tag::builder().key("telchar-capacity").value("999").build())
        .build();
    let mut stopped = instance.clone();
    stopped.state = Some(
        InstanceState::builder()
            .name(InstanceStateName::Stopped)
            .build(),
    );
    let mut untagged = instance.clone();
    untagged.tags = None;
    let mut missing = instance.clone();
    missing.private_ip_address = None;
    let members = telchar::service::static_ssh_ec2::map_instances(
        &config.static_ssh_ec2()[0],
        &[instance, stopped, untagged, missing],
    )
    .unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].destination(), "telchar@10.0.0.1");
    assert_eq!(members[0].maximum_concurrent_builds(), 2);
    assert!(members[0].target().name().contains("i-0123456789abcdef0"));
}

#[test]
fn refresh_signs_filtered_requests_and_preserves_inventory_after_failure() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let root = tempfile::tempdir().unwrap();
    let path = configuration(
        root.path(),
        "source = \"ec2\"\nregion = \"us-east-1\"\ntags = { telchar-pool = \"builders\" }",
    );
    let config = ServiceConfig::load_from_default(&path).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for index in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let headers = String::from_utf8(request).unwrap().to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            assert!(headers.contains("authorization: aws4-hmac-sha256"));
            assert!(body.contains("Action=DescribeInstances"));
            assert!(body.contains("instance-state-name"));
            assert!(body.contains("tag%3Atelchar-pool"));
            if index == 1 {
                assert!(body.contains("NextToken=page-two"));
            }
            let (status, body) = match index {
                0 => (
                    "200 OK",
                    "<DescribeInstancesResponse xmlns=\"http://ec2.amazonaws.com/doc/2016-11-15/\"><reservationSet><item><instancesSet><item><instanceId>i-0123456789abcdef0</instanceId><instanceState><name>running</name></instanceState><privateIpAddress>10.0.0.1</privateIpAddress><tagSet><item><key>telchar-pool</key><value>builders</value></item></tagSet></item></instancesSet></item></reservationSet><nextToken>page-two</nextToken></DescribeInstancesResponse>",
                ),
                1 => (
                    "200 OK",
                    "<DescribeInstancesResponse xmlns=\"http://ec2.amazonaws.com/doc/2016-11-15/\"><reservationSet/></DescribeInstancesResponse>",
                ),
                3 => (
                    "200 OK",
                    "<DescribeInstancesResponse xmlns=\"http://ec2.amazonaws.com/doc/2016-11-15/\"><reservationSet/></DescribeInstancesResponse>",
                ),
                _ => (
                    "503 Service Unavailable",
                    "<Response><Errors><Error><Code>Unavailable</Code><Message>unavailable</Message></Error></Errors></Response>",
                ),
            };
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let client = aws_sdk_ec2::Client::from_conf(
        aws_sdk_ec2::config::Builder::new()
            .behavior_version_latest()
            .region(aws_sdk_ec2::config::Region::new("us-east-1"))
            .credentials_provider(aws_credential_types::Credentials::new(
                "AKIATEST", "secret", None, None, "test",
            ))
            .endpoint_url(endpoint)
            .retry_config(aws_sdk_ec2::config::retry::RetryConfig::disabled())
            .build(),
    );
    let mut discovery = telchar::service::static_ssh_ec2::Ec2SshDiscovery::with_client(
        config.static_ssh_ec2()[0].clone(),
        client,
    );
    assert_eq!(runtime.block_on(discovery.refresh()).unwrap(), 1);
    assert_eq!(discovery.inventory()[0].destination(), "telchar@10.0.0.1");
    assert!(runtime.block_on(discovery.refresh()).is_err());
    assert_eq!(discovery.inventory().len(), 1);
    assert_eq!(runtime.block_on(discovery.refresh()).unwrap(), 0);
    assert!(discovery.inventory().is_empty());
    server.join().unwrap();
}

#[test]
fn public_addresses_are_explicit_and_missing_addresses_are_skipped() {
    use aws_sdk_ec2::types::{Instance, InstanceState, InstanceStateName, Tag};
    let root = tempfile::tempdir().unwrap();
    let path = configuration(
        root.path(),
        "source = \"ec2\"\nregion = \"us-east-1\"\naddress = \"public-ip\"\ntags = { telchar-pool = \"builders\" }",
    );
    let config = ServiceConfig::load_from_default(&path).unwrap();
    let instance = Instance::builder()
        .instance_id("i-0123456789abcdef0")
        .state(
            InstanceState::builder()
                .name(InstanceStateName::Running)
                .build(),
        )
        .private_ip_address("10.0.0.1")
        .public_ip_address("203.0.113.1")
        .tags(Tag::builder().key("telchar-pool").value("builders").build())
        .build();
    let members = telchar::service::static_ssh_ec2::map_instances(
        &config.static_ssh_ec2()[0],
        std::slice::from_ref(&instance),
    )
    .unwrap();
    assert_eq!(members[0].destination(), "telchar@203.0.113.1");
    let mut missing = instance.clone();
    missing.public_ip_address = None;
    assert!(
        telchar::service::static_ssh_ec2::map_instances(&config.static_ssh_ec2()[0], &[missing])
            .unwrap()
            .is_empty()
    );
    assert!(
        telchar::service::static_ssh_ec2::map_instances(
            &config.static_ssh_ec2()[0],
            &[instance.clone(), instance]
        )
        .is_err()
    );
}

#[test]
fn rejects_unbounded_or_ambiguous_ec2_configuration() {
    for fields in [
        "region = \"\"\ntags = { pool = \"builders\" }",
        "region = \"us-east-1\"",
        "region = \"us-east-1\"\ntags = { pool = \"*\" }",
        "region = \"us-east-1\"\ntags = { pool = \"builders\" }\naddress = \"automatic\"",
        "region = \"us-east-1\"\ntags = { pool = \"builders\" }\nrequest_timeout_seconds = 0",
        "region = \"us-east-1\"\ntags = { pool = \"builders\" }\nrefresh_interval_seconds = 0",
        "region = \"us-east-1\"\ntags = { pool = \"builders\" }\nendpoint = \"http://example.com\"",
        "region = \"us-east-1\"\ntags = { pool = \"builders\" }\n[backends.ssh.builders.credentials]\naccess_key_id_file = \"relative\"\nsecret_access_key_file = \"/secret\"",
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = configuration(root.path(), &format!("source = \"ec2\"\n{fields}"));
        assert!(
            ServiceConfig::load_from_default(&path).is_err(),
            "accepted: {fields}"
        );
    }
}

#[test]
fn rejects_pool_names_without_room_for_instance_identity() {
    let root = tempfile::tempdir().unwrap();
    let path = configuration(
        root.path(),
        "source = \"ec2\"\nregion = \"us-east-1\"\ntags = { pool = \"builders\" }",
    );
    let contents = fs::read_to_string(&path).unwrap().replace(
        "backends.ssh.builders",
        &format!("backends.ssh.{}", "a".repeat(256)),
    );
    fs::write(&path, contents).unwrap();
    assert!(ServiceConfig::load_from_default(&path).is_err());
}

#[test]
fn rejects_ec2_fields_on_other_sources() {
    let root = tempfile::tempdir().unwrap();
    let path = configuration(
        root.path(),
        "source = \"static\"\nregion = \"us-east-1\"\n[backends.ssh.builders.one]\naddress = \"builder.example\"",
    );
    assert!(ServiceConfig::load_from_default(&path).is_err());
}

#[test]
fn background_discovery_shuts_down_with_unavailable_credentials() {
    let root = tempfile::tempdir().unwrap();
    let path = configuration(
        root.path(),
        &format!(
            r#"
source = "ec2"
region = "us-east-1"
request_timeout_seconds = 1
tags = {{ telchar-pool = "builders" }}
[backends.ssh.builders.credentials]
access_key_id_file = "{}/missing-key"
secret_access_key_file = "{}/missing-secret"
"#,
            root.path().display(),
            root.path().display()
        ),
    );
    let config = ServiceConfig::load_from_default(&path).unwrap();
    let mut service = telchar::service::static_ssh_ec2::Ec2SshDiscoveryService::start(
        config.static_ssh_ec2().to_vec(),
    )
    .unwrap();
    service.shutdown().unwrap();
}

#[test]
fn reads_protected_credentials_and_rejects_exposed_files() {
    use aws_credential_types::provider::ProvideCredentials;
    use telchar::service::config::Ec2CredentialsConfig;
    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("key");
    let secret = root.path().join("secret");
    fs::write(&key, "AKIATEST\n").unwrap();
    fs::write(&secret, "secret-value\n").unwrap();
    for path in [&key, &secret] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let config = Ec2CredentialsConfig {
        access_key_id_file: key,
        secret_access_key_file: secret.clone(),
        session_token_file: None,
    };
    let provider = telchar::service::static_ssh_ec2::CredentialFiles::new(config);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let credentials = runtime.block_on(provider.provide_credentials()).unwrap();
    assert_eq!(credentials.access_key_id(), "AKIATEST");
    assert_eq!(credentials.secret_access_key(), "secret-value");
    fs::write(&secret, "rotated-secret\n").unwrap();
    assert_eq!(
        runtime
            .block_on(provider.provide_credentials())
            .unwrap()
            .secret_access_key(),
        "rotated-secret"
    );
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
    let error = runtime
        .block_on(provider.provide_credentials())
        .unwrap_err();
    assert!(!error.to_string().contains("rotated-secret"));
}

#[test]
fn loads_ec2_membership_with_instance_role_credentials() {
    let root = tempfile::tempdir().unwrap();
    let path = configuration(
        root.path(),
        r#"
source = "ec2"
region = "us-east-1"
[backends.ssh.builders.tags]
telchar-pool = "builders"
"#,
    );
    let config =
        ServiceConfig::load_from_default(&path).expect("EC2 discovery configuration loads");
    let pool = &config.static_ssh_ec2()[0];
    assert_eq!(pool.region(), "us-east-1");
    assert_eq!(
        pool.address(),
        telchar::service::config::Ec2Address::PrivateIp
    );
    assert_eq!(pool.tags()["telchar-pool"], "builders");
    assert!(pool.credentials().is_none());
}
