//! Tests store export process contracts and failure boundaries, including oversized helper stderr is drained bounded and helper is reaped.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use telchar::store::export::{NixStoreExportBackend, StoreExportBackend, StoreExportRequest};

#[test]
fn oversized_helper_stderr_is_drained_bounded_and_helper_is_reaped() {
    let fixture = HelperFixture::create(
        r#"#!/bin/sh
printf '%070000d' 0 >&2
exit 1
"#,
    );
    let mut backend = NixStoreExportBackend::new(
        fixture.helper.clone(),
        "unix:///run/nix-daemon.sock",
        Vec::<(String, String)>::new(),
    );
    let request = export_request();
    let mut output = Vec::new();

    let error = backend
        .export_nar(&request, 0, &mut output)
        .expect_err("oversized stderr must fail");

    assert!(error.to_string().contains("output exceeds limit"));
    fixture.assert_recorded_process_reaped();
}

#[test]
fn writer_failure_kills_and_reaps_helper() {
    let fixture = HelperFixture::create(
        r#"#!/bin/sh
trap 'exit 0' TERM INT
while :; do
  printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'
done
"#,
    );
    let mut backend = NixStoreExportBackend::new(
        fixture.helper.clone(),
        "unix:///run/nix-daemon.sock",
        Vec::<(String, String)>::new(),
    );
    let request = export_request();
    let mut writer = FailingWriter;

    let error = backend
        .export_nar(&request, 0, &mut writer)
        .expect_err("writer failure must stop helper");

    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe, "{error}");
    fixture.assert_recorded_process_reaped();
}

#[test]
fn panicking_writer_terminates_export_helper() {
    let fixture = HelperFixture::create(
        r#"#!/bin/sh
trap 'exit 0' TERM INT
while :; do
  printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'
done
"#,
    );
    let mut backend = NixStoreExportBackend::new(
        fixture.helper.clone(),
        "unix:///run/nix-daemon.sock",
        Vec::<(String, String)>::new(),
    );
    let request = export_request();
    let mut writer = PanickingWriter;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = backend.export_nar(&request, 0, &mut writer);
    }));

    assert!(result.is_err(), "writer did not panic");
    fixture.assert_recorded_process_reaped();
}

#[test]
fn killed_owner_terminates_export_helper() {
    const CHILD_ENV: &str = "TELCHAR_EXPORT_OWNER_CHILD";
    const HELPER_ENV: &str = "TELCHAR_EXPORT_OWNER_HELPER";
    const EVIDENCE_ENV: &str = "TELCHAR_EXPORT_OWNER_EVIDENCE";

    if std::env::var_os(CHILD_ENV).is_some() {
        let helper = PathBuf::from(std::env::var_os(HELPER_ENV).expect("helper path"));
        let evidence = PathBuf::from(std::env::var_os(EVIDENCE_ENV).expect("evidence path"));
        let mut backend = NixStoreExportBackend::new(
            helper,
            "unix:///run/nix-daemon.sock",
            vec![(
                "TELCHAR_EXPORT_HELPER_EVIDENCE".to_owned(),
                evidence.display().to_string(),
            )],
        );
        let mut output = io::sink();
        let _ = backend.export_nar(&export_request(), 0, &mut output);
        return;
    }

    let root = temporary_root();
    let evidence = root.join("helper.pid");
    let fixture = HelperFixture::create_in(
        &root,
        r#"#!/bin/sh
printf '%s' "$$" > "$TELCHAR_EXPORT_HELPER_EVIDENCE"
trap 'exit 0' TERM INT
while :; do sleep 1; done
"#,
    );
    let mut owner = Command::new(std::env::current_exe().expect("test executable path"))
        .args(["killed_owner_terminates_export_helper", "--exact"])
        .env(CHILD_ENV, "1")
        .env(HELPER_ENV, &fixture.helper)
        .env(EVIDENCE_ENV, &evidence)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("export owner starts");
    wait_for_file(&evidence);
    let pid = read_pid(&evidence);

    owner.kill().expect("export owner killed");
    owner.wait().expect("export owner reaped");
    wait_for_process_exit(pid);

    fixture.cleanup();
}

fn export_request() -> StoreExportRequest {
    StoreExportRequest {
        version: 1,
        store_uri: "unix:///run/nix-daemon.sock".to_owned(),
        path: PathBuf::from("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-fixture"),
    }
}

struct PanickingWriter;

impl Write for PanickingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        panic!("writer panicked")
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "writer failed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct HelperFixture {
    root: PathBuf,
    helper: PathBuf,
    evidence: PathBuf,
}

impl HelperFixture {
    fn create(script: &str) -> Self {
        let root = temporary_root();
        Self::create_in(&root, script)
    }

    fn create_in(root: &Path, script: &str) -> Self {
        fs::create_dir_all(root).expect("helper root creates");
        let helper = root.join("helper.sh");
        let evidence = root.join("helper.pid");
        let script = format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > \"{}\"\n{}",
            evidence.display(),
            script.strip_prefix("#!/bin/sh\n").unwrap_or(script)
        );
        fs::write(&helper, script).expect("helper writes");
        let mut permissions = fs::metadata(&helper)
            .expect("helper metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&helper, permissions).expect("helper executable");
        Self {
            root: root.to_path_buf(),
            helper,
            evidence,
        }
    }

    fn assert_recorded_process_reaped(&self) {
        wait_for_file(&self.evidence);
        wait_for_process_exit(read_pid(&self.evidence));
    }

    fn cleanup(&self) {
        fs::remove_dir_all(&self.root).expect("helper fixture cleans");
    }
}

impl Drop for HelperFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn temporary_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "telchar-store-export-process-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ))
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.is_file() {
        assert!(Instant::now() < deadline, "evidence file was not created");
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_pid(path: &Path) -> rustix::process::Pid {
    let raw: i32 = fs::read_to_string(path)
        .expect("pid evidence reads")
        .parse()
        .expect("pid evidence parses");
    rustix::process::Pid::from_raw(raw).expect("positive pid")
}

fn wait_for_process_exit(pid: rustix::process::Pid) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if rustix::process::test_kill_process(pid).is_err() {
            return;
        }
        assert!(Instant::now() < deadline, "helper process leaked: {pid:?}");
        thread::sleep(Duration::from_millis(10));
    }
}
