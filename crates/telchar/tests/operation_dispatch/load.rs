//! Exercises distinct real builds across quota subjects through production IPC.

use super::*;

#[test]
fn distinct_builds_preserve_four_subject_accounting() {
    mixed_batches(Duration::ZERO);
}

#[test]
#[ignore = "opt-in twenty-minute isolated workload; requires external resource monitor"]
fn sustained_mixed_builds_preserve_accounting() {
    let seconds = std::env::var("TELCHAR_SOAK_SECONDS")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(1200);
    assert!((1..=1200).contains(&seconds));
    mixed_batches(Duration::from_secs(seconds));
}

fn mixed_batches(duration: Duration) {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("telchar.toml");
    let mut source = String::from(
        "[scheduling.default]\nmaximum_queued_builds = 64\nmaximum_active_builds = 2\n\n[backends.local]\nname = \"local\"\nsystem = \"x86_64-linux\"\nmaximum_concurrent_builds = 4\n",
    );
    for subject in 0..4 {
        source.push_str(&format!("\n[identity.credentials.\"ssh-pubkey:SHA256:load{subject}\"]\naudit_subject = \"load{subject}\"\nquota_subject = \"load{subject}\"\n"));
    }
    fs::write(&config, source).unwrap();
    let mut fixture = FrontendFixture::spawn_multi_with_store(
        "unix:///nix/var/nix/daemon-socket/socket",
        [("TELCHAR_CONFIG", config.display().to_string())],
    );
    drop(fixture.frontend.stdin.take());
    drop(fixture.frontend.stdout.take());
    assert!(fixture.frontend.wait().unwrap().success());
    fixture.database.connect().batch_execute("
        INSERT INTO shared_builds (derivation_path,request_digest,state,backend_name,backend_kind,execution_recovery,cancellation,log_recovery,expected_outputs,result_metadata,failure_classification,created_at,started_at,completed_at,expires_at)
        SELECT '/nix/store/' || lpad(n::text,32,'0') || '-aged-load.drv',decode(repeat('01',32),'hex'),'failed','local','local','output-only','connection-bound','live-only',ARRAY['/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-aged-output'],'{}'::jsonb,'fixture-failure',now()-interval '60 days',now()-interval '50 days',now()-interval '40 days',now()-interval '39 days'
        FROM generate_series(1,10000) n;
        ANALYZE shared_builds;
    ").unwrap();
    let soak_started = Instant::now();
    let mut batches = 0;
    loop {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let release = root.path().join(format!("release-{sequence}"));
        let sleep = Command::new("sh")
            .args(["-c", "command -v sleep"])
            .output()
            .unwrap();
        assert!(sleep.status.success());
        let sleep = String::from_utf8(sleep.stdout).unwrap();
        let dd = Command::new("sh")
            .args(["-c", "command -v dd"])
            .output()
            .unwrap();
        assert!(dd.status.success());
        let dd = String::from_utf8(dd.stdout).unwrap();
        let mut builds = Vec::new();
        for index in 0..8 {
            let action = match index {
                0 => format!("{} 1; printf load-{index} > $out", sleep.trim()),
                1 => format!("{} 10; printf load-{index} > $out", sleep.trim()),
                2 => format!(
                    "{} if=/dev/zero of=$out bs=1048576 count=32 status=none; status=$?; exit $status",
                    dd.trim()
                ),
                _ => format!("printf load-{index} > $out"),
            };
            let expression = format!(
                r#"derivation {{ name = "telchar-load-{sequence}-{index}"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "remaining=500; while [ ! -e {} ]; do remaining=$((remaining-1)); [ $remaining -gt 0 ] || exit 1; {} 0.01; done; {action}" ]; }}"#,
                release.display(),
                sleep.trim()
            );
            let output = Command::new("nix-instantiate")
                .args(["--store", "local", "--expr", &expression])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let drv = String::from_utf8(output.stdout).unwrap().trim().to_owned();
            let kind = telchar::backend::BackendKind::Local;
            builds.push(
                telchar::build::BuildRequest::from_stored_derivation(
                    drv.as_bytes(),
                    &fs::read(&drv).unwrap(),
                    &[telchar::backend::BackendTarget::new(
                        "local",
                        kind,
                        "x86_64-linux",
                        [] as [&str; 0],
                    )
                    .unwrap()],
                )
                .unwrap(),
            );
        }
        let started = Instant::now();
        let mut clients = Vec::new();
        let (ready, readiness) = std::sync::mpsc::channel();
        for index in 0..32 {
            let build = builds[index % 8].clone();
            let ready = ready.clone();
            let mut client = fixture.spawn_frontend_with_key(&format!("SHA256:load{}", index % 4));
            clients.push(thread::spawn(move || {
                let mut input = client.stdin.take().unwrap();
                let mut output = client.stdout.take().unwrap();
                complete_handshake(&mut input, &mut output);
                super::build_failure::send_failure_build(&mut input, &build, "", false);
                ready.send(()).unwrap();
                let request_started = Instant::now();
                read_build_success(output).unwrap();
                assert!(
                    request_started.elapsed() < Duration::from_secs(60),
                    "build exceeds qualification bound"
                );
                drop(input);
                assert!(client.wait().unwrap().success());
                let path = String::from_utf8(build.expected_outputs()[0].1.clone()).unwrap();
                if index % 8 == 2 {
                    let mut file = fs::File::open(path).unwrap();
                    assert_eq!(file.metadata().unwrap().len(), 32 * 1024 * 1024);
                    let mut buffer = [0; 65536];
                    loop {
                        let length = file.read(&mut buffer).unwrap();
                        if length == 0 {
                            break;
                        }
                        assert!(buffer[..length].iter().all(|byte| *byte == 0));
                    }
                } else {
                    assert_eq!(
                        fs::read(path).unwrap(),
                        format!("load-{}", index % 8).as_bytes()
                    );
                }
            }));
        }
        drop(ready);
        let mut submitted = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while submitted < 32 {
            if readiness
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_err()
            {
                break;
            }
            submitted += 1;
        }
        let status = fs::read_to_string(format!("/proc/{}/status", fixture.daemon.id())).unwrap();
        let value = |name: &str| -> u64 {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        let rss_kib = value("VmRSS:");
        let threads = value("Threads:");
        let fds = fs::read_dir(format!("/proc/{}/fd", fixture.daemon.id()))
            .unwrap()
            .count();
        fs::write(&release, "release").unwrap();
        assert!(
            rss_kib < 8 * 1024 * 1024,
            "gateway RSS exceeds local ceiling: {rss_kib}KiB"
        );
        eprintln!("LOAD_GATEWAY_SNAPSHOT rss_kib={rss_kib} threads={threads} fds={fds}");
        for client in clients {
            client.join().unwrap();
        }
        assert_eq!(submitted, 32, "every client submits before release");
        let mut connection = fixture.database.connect();
        assert_eq!(
            connection
                .query_one(
                    "SELECT count(*) FROM shared_build_attempts WHERE state='succeeded'",
                    &[]
                )
                .unwrap()
                .get::<_, i64>(0),
            (batches + 1) * 8
        );
        let subjects = connection.query("SELECT quota_subject,count(*) FROM build_requests GROUP BY quota_subject ORDER BY quota_subject", &[]).unwrap();
        assert_eq!(subjects.len(), 4);
        for subject in subjects {
            assert_eq!(subject.get::<_, i64>(1), (batches + 1) * 8);
        }
        eprintln!(
            "DISTINCT_LOAD builds=8 clients=32 subjects=4 elapsed_ms={}",
            started.elapsed().as_millis()
        );
        let active: i64 = connection.query_one("SELECT count(*) FROM shared_builds WHERE state IN ('claimed','running','collecting')", &[]).unwrap().get(0);
        assert_eq!(active, 0);
        let history: i64 = connection.query_one("SELECT count(*) FROM shared_builds WHERE state='failed' AND completed_at < now()-interval '30 days'", &[]).unwrap().get(0);
        assert_eq!(history, 10000);
        assert!(fixture.daemon.try_wait().unwrap().is_none());
        batches += 1;
        eprintln!(
            "SOAK_PROGRESS batches={batches} elapsed_seconds={}",
            soak_started.elapsed().as_secs()
        );
        if soak_started.elapsed() >= duration {
            break;
        }
    }
    eprintln!(
        "SOAK_COMPLETE batches={batches} executions={} requests={} elapsed_seconds={}",
        batches * 8,
        batches * 32,
        soak_started.elapsed().as_secs()
    );
    fixture.finish();
}
