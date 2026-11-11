//! Tests bounded post-build hook command execution without shell interpolation.

use std::fs;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use telchar::service::post_build_hook::PostBuildHook;

#[test]
fn configuration_accepts_hook_names_and_rejects_duplicate_sections() {
    use telchar::service::config::ServiceConfig;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("telchar.toml");
    let section = "executable = \"/bin/sh\"\narguments = [\"-c\", \"cat >/dev/null\"]\ntimeout_seconds = 5\nmaximum_input_bytes = 1024\n";
    let mut hooks = Vec::new();
    for name in ["post_build_hook", "cache_publication"] {
        fs::write(&path, format!("[{name}]\n{section}")).unwrap();
        let config = ServiceConfig::load_from_default(&path).unwrap();
        let hook = config.post_build_hook().unwrap().clone();
        hook.run(&["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one".into()])
            .unwrap();
        hooks.push(hook);
    }
    assert_eq!(hooks[0], hooks[1]);
    fs::write(
        &path,
        format!("[post_build_hook]\n{section}[cache_publication]\n{section}"),
    )
    .unwrap();
    assert!(ServiceConfig::load_from_default(&path).is_err());
}

#[derive(Clone, Default)]
struct LogOutput(Arc<Mutex<Vec<u8>>>);

impl Write for LogOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn publisher_logs_bounded_stdout_and_stderr_at_debug() {
    let output = LogOutput::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let publisher = PostBuildHook::new(
        "/bin/sh", ["-c", "cat >/dev/null; printf stdout-marker; printf stderr-marker >&2; head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2"],
        Duration::from_secs(5), 1024,
    ).unwrap();
    tracing::subscriber::set_global_default(subscriber).unwrap();
    publisher
        .run(&["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one".into()])
        .unwrap();
    let bytes = output.0.lock().unwrap();
    let logs = String::from_utf8_lossy(&bytes);
    assert!(logs.contains("stdout-marker"), "{logs}");
    assert!(logs.contains("stderr-marker"), "{logs}");
    assert!(logs.contains("post_build_hook.output"), "{logs}");
    assert!(logs.contains("truncated=true"), "{logs}");
    assert!(bytes.len() < 70000, "diagnostic output is bounded");
}

#[test]
fn publisher_timeout_bounds_a_command_that_does_not_read_input() {
    let publisher = PostBuildHook::new(
        "/bin/sh",
        [
            "-c",
            "while :; do printf diagnostic; printf diagnostic >&2; done",
        ],
        Duration::from_millis(100),
        131072,
    )
    .unwrap();
    let started = std::time::Instant::now();
    assert!(publisher.run(&["x".repeat(100000)]).is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn publisher_passes_validated_outputs_through_stdin() {
    let root = tempfile::tempdir().expect("fixture creates");
    let input = root.path().join("input");
    let publisher = PostBuildHook::new(
        "/bin/sh",
        ["-c", &format!("cat > '{}'", input.display())],
        Duration::from_secs(5),
        1024,
    )
    .expect("publisher config validates");

    publisher
        .run(&[
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one".to_owned(),
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-two".to_owned(),
        ])
        .expect("publication succeeds");

    assert_eq!(
        fs::read_to_string(input).expect("publisher input reads"),
        "[\"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one\",\"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-two\"]\n"
    );
}

#[test]
fn publisher_rejects_relative_executable_and_bounds_diagnostics() {
    assert!(
        PostBuildHook::new(
            "publisher",
            std::iter::empty::<&str>(),
            Duration::from_secs(1),
            1024,
        )
        .is_err()
    );
    let publisher = PostBuildHook::new(
        "/bin/sh",
        ["-c", "printf '%02048d' 0 >&2; exit 1"],
        Duration::from_secs(1),
        64,
    )
    .expect("publisher config validates");

    let error = publisher
        .run(&["/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one".to_owned()])
        .expect_err("publisher failure reports");

    assert_eq!(error.to_string(), "post-build hook command failed");
}
