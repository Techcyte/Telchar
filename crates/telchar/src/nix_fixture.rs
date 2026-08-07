use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub enum TrustMode {
    Trusted,
    Untrusted,
}

pub struct NixDaemon {
    child: Child,
    diagnostic_operations: Option<Vec<u64>>,
    environment: BTreeMap<&'static str, String>,
    socket_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorePathInfo {
    pub path: PathBuf,
    pub nar_hash: String,
    pub nar_size: u64,
    pub references: Vec<PathBuf>,
    pub deriver: Option<PathBuf>,
    pub content_address: Option<String>,
}

#[derive(serde::Deserialize)]
struct NixPathInfo {
    #[serde(rename = "narHash")]
    nar_hash: String,
    #[serde(rename = "narSize")]
    nar_size: u64,
    references: Vec<String>,
    deriver: Option<String>,
    ca: Option<String>,
}

pub struct NixFixture {
    cleanup_guard: Option<Child>,
    root: PathBuf,
    config_path: PathBuf,
    private_key_path: PathBuf,
    public_key_path: PathBuf,
    state_dir: PathBuf,
    store_dir: PathBuf,
    log_dir: PathBuf,
    config_dir: PathBuf,
    socket_path: PathBuf,
    temp_dir: PathBuf,
}

impl NixFixture {
    pub fn create() -> io::Result<Self> {
        let lifecycle = tracing::info_span!(
            "nix_fixture.lifecycle",
            fixture = "isolated",
            client = "nix"
        );
        let _entered = lifecycle.enter();
        tracing::info!(
            event = "nix.fixture.setup.started",
            "Nix fixture setup started"
        );
        let root = std::env::temp_dir().join(format!(
            "telchar-nix-fixture-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let state_dir = root.join("state");
        let store_dir = root.join("store");
        let log_dir = root.join("log");
        let config_dir = root.join("config");
        let socket_path = root.join("socket").join("daemon.sock");
        let temp_dir = root.join("tmp");
        let config_path = root.join("nix.conf");
        let private_key_path = root.join("client-key");
        let public_key_path = root.join("client-key.pub");

        let setup =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/create-nix-fixture.sh");
        let setup = Command::new(setup).arg(&root).output()?;
        if !setup.status.success() {
            let _ = fs::remove_dir_all(&root);
            tracing::error!(
                event = "nix.fixture.setup.failed",
                "Nix fixture setup failed"
            );
            let stderr = String::from_utf8_lossy(&setup.stderr);
            let reason = stderr.trim();
            return Err(io::Error::other(if reason.is_empty() {
                "fixture setup failed".to_owned()
            } else {
                format!("fixture setup failed: {reason}")
            }));
        }

        let cleanup_guard = start_cleanup_guard(&root)?;
        tracing::info!(
            event = "nix.fixture.setup.finished",
            state = "isolated",
            "Nix fixture setup finished"
        );
        Ok(Self {
            cleanup_guard: Some(cleanup_guard),
            root,
            config_path,
            private_key_path,
            public_key_path,
            state_dir,
            store_dir,
            log_dir,
            config_dir,
            socket_path,
            temp_dir,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn private_key_path(&self) -> &Path {
        &self.private_key_path
    }

    pub fn public_key_path(&self) -> &Path {
        &self.public_key_path
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn store_dir(&self) -> &Path {
        &self.store_dir
    }

    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    pub fn start_daemon(&self, mode: TrustMode) -> io::Result<NixDaemon> {
        self.start_daemon_with_diagnostics(mode, false)
    }

    pub fn start_diagnostic_daemon(&self, mode: TrustMode) -> io::Result<NixDaemon> {
        self.start_daemon_with_diagnostics(mode, true)
    }

    fn start_daemon_with_diagnostics(
        &self,
        mode: TrustMode,
        diagnostics_enabled: bool,
    ) -> io::Result<NixDaemon> {
        let user = fixture_user()?;
        if user == "root" {
            return Err(io::Error::other(
                "fixture daemon requires a non-root client user",
            ));
        }
        let trusted_users = match mode {
            TrustMode::Trusted => user,
            TrustMode::Untrusted => "root".to_owned(),
        };
        let config = format!(
            "{}\nallowed-users = *\nbuild-users-group =\nsandbox = false\ntrusted-users = {trusted_users}\nsubstituters =\nbuild-hook =\n",
            fs::read_to_string(&self.config_path)?
        );
        let environment = self.daemon_environment(config);
        let mut command = Command::new("nix-daemon");
        command
            .envs(&environment)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if diagnostics_enabled {
            command.arg("--debug");
        }
        let mut child = command.spawn()?;
        fs::write(self.root.join("daemon.pid"), child.id().to_string())?;

        for _ in 0..100 {
            if self.socket_path.exists() {
                tracing::info!(
                    event = "nix.fixture.daemon.started",
                    trust_mode = ?mode,
                    "Fixture daemon started"
                );
                return Ok(NixDaemon {
                    child,
                    diagnostic_operations: diagnostics_enabled.then(Vec::new),
                    environment,
                    socket_path: self.socket_path.clone(),
                });
            }
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "fixture daemon exited before binding socket: {status}"
                )));
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "fixture daemon did not bind socket",
        ))
    }

    pub fn environment(&self) -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            (
                "NIX_CONFIG",
                fs::read_to_string(&self.config_path).expect("fixture configuration exists"),
            ),
            ("TMPDIR", self.temp_dir.display().to_string()),
            ("NIX_STORE_DIR", self.store_dir.display().to_string()),
            ("NIX_STATE_DIR", self.state_dir.display().to_string()),
            ("NIX_LOG_DIR", self.log_dir.display().to_string()),
            ("NIX_CONF_DIR", self.config_dir.display().to_string()),
            (
                "NIX_DAEMON_SOCKET_PATH",
                self.socket_path.display().to_string(),
            ),
            ("NIX_USER_CONF_FILES", "/dev/null".to_owned()),
        ])
    }

    fn daemon_environment(&self, config: String) -> BTreeMap<&'static str, String> {
        let mut environment = self.environment();
        environment.insert("NIX_CONFIG", config);
        environment
    }

    pub fn cleanup(mut self) -> io::Result<()> {
        let lifecycle =
            tracing::info_span!("nix_fixture.cleanup", fixture = "isolated", client = "nix");
        let _entered = lifecycle.enter();
        tracing::info!(
            event = "nix.fixture.cleanup.started",
            "Nix fixture cleanup started"
        );
        stop_cleanup_guard(&mut self.cleanup_guard);
        remove_fixture_root(&self.root)?;
        tracing::info!(
            event = "nix.fixture.cleanup.finished",
            "Nix fixture cleanup finished"
        );
        Ok(())
    }
}

impl Drop for NixFixture {
    fn drop(&mut self) {
        stop_cleanup_guard(&mut self.cleanup_guard);
        if let Err(error) = remove_fixture_root(&self.root) {
            tracing::error!(
                event = "nix.fixture.cleanup.failed",
                error = %error,
                "Nix fixture cleanup failed"
            );
        }
    }
}

impl NixDaemon {
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn store_url(&self) -> String {
        format!("unix://{}", self.socket_path.display())
    }

    pub fn is_valid_path(&self, path: &Path) -> io::Result<bool> {
        let output = self.path_info_output(path)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("is not valid")
                || stderr.contains("does not exist")
                || stderr.contains("is not a valid store path")
            {
                return Ok(false);
            }
            return Err(io::Error::other(format!(
                "fixture daemon path-info query failed: {}",
                stderr.trim()
            )));
        }
        if output.stdout.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture daemon path-info response exceeds limit",
            ));
        }
        let entries: BTreeMap<String, Option<serde_json::Value>> =
            serde_json::from_slice(&output.stdout).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid path-info JSON")
            })?;
        Ok(entries
            .get(path.to_string_lossy().as_ref())
            .is_some_and(Option::is_some))
    }

    pub fn query_path_info(&self, path: &Path) -> io::Result<StorePathInfo> {
        let output = self.path_info_output(path)?;
        if !output.status.success() {
            return Err(io::Error::other("fixture daemon path-info query failed"));
        }
        if output.stdout.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture daemon path-info response exceeds limit",
            ));
        }
        let entries: BTreeMap<String, NixPathInfo> = serde_json::from_slice(&output.stdout)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid path-info JSON"))?;
        let info = entries
            .get(path.to_string_lossy().as_ref())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "path-info response omitted path",
                )
            })?;
        Ok(StorePathInfo {
            path: path.to_path_buf(),
            nar_hash: info.nar_hash.clone(),
            nar_size: info.nar_size,
            references: info.references.iter().map(PathBuf::from).collect(),
            deriver: info.deriver.as_deref().map(PathBuf::from),
            content_address: info.ca.clone(),
        })
    }

    fn path_info_output(&self, path: &Path) -> io::Result<std::process::Output> {
        Command::new("nix")
            .envs(&self.environment)
            .args(["--store", &self.store_url(), "path-info", "--json"])
            .arg(path)
            .output()
    }

    pub fn trusted(&mut self) -> io::Result<bool> {
        let output = Command::new("nix")
            .envs(&self.environment)
            .args(["--store", &self.store_url(), "store", "info", "--json"])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("fixture daemon store-info failed"));
        }
        match String::from_utf8_lossy(&output.stdout).as_ref() {
            value if value.contains("\"trusted\":true") => Ok(true),
            value if value.contains("\"trusted\":false") => Ok(false),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture daemon did not report trust status",
            )),
        }
    }

    pub fn diagnostic_operations(&self) -> io::Result<Vec<u64>> {
        self.diagnostic_operations
            .clone()
            .ok_or_else(|| io::Error::other("fixture daemon diagnostics are disabled"))
    }

    /// Runs the fixture's fixed input-addressed build through this daemon only.
    pub fn build_classic_derivation(&mut self) -> io::Result<PathBuf> {
        let expression = "derivation { name = \"telchar-classic-fixture\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [ \"-c\" \"printf telchar-classic-fixture > \\\"$out\\\"\" ]; }";
        let mut command = Command::new("nix");
        command.envs(&self.environment).args([
            "--store",
            &self.store_url(),
            "build",
            "--impure",
            "--expr",
            expression,
            "--no-link",
            "--print-out-paths",
        ]);
        if self.diagnostic_operations.is_some() {
            command.arg("--debug");
        }
        let output = command.output()?;
        if let Some(operations) = &mut self.diagnostic_operations {
            *operations = diagnostic_operation_codes(&output.stderr);
        }
        if !output.status.success() {
            return Err(io::Error::other("fixture daemon classic build failed"));
        }
        let path = String::from_utf8(output.stdout).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "fixture output is not UTF-8")
        })?;
        let path = path.trim();
        if path.is_empty() || path.contains('\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture daemon did not report exactly one output path",
            ));
        }
        Ok(PathBuf::from(path))
    }

    pub fn stop(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        let _ = self.child.wait()?;
        tracing::info!(
            event = "nix.fixture.daemon.stopped",
            "Fixture daemon stopped"
        );
        Ok(())
    }
}

impl Drop for NixDaemon {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::error!(
                event = "nix.fixture.daemon.cleanup_failed",
                error = %error,
                "Fixture daemon cleanup failed"
            );
        }
    }
}

fn start_cleanup_guard(root: &Path) -> io::Result<Child> {
    let script = r#"
parent=$1
root=$2
while kill -0 "$parent" 2>/dev/null; do sleep 0.05; done
if test -f "$root/daemon.pid"; then
    daemon=$(cat "$root/daemon.pid")
    kill "$daemon" 2>/dev/null || true
    sleep 0.05
    kill -KILL "$daemon" 2>/dev/null || true
fi
rm -rf "$root"
"#;
    Command::new("sh")
        .args([
            "-c",
            script,
            "telchar-nix-fixture-cleanup",
            &std::process::id().to_string(),
            &root.display().to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn stop_cleanup_guard(guard: &mut Option<Child>) {
    if let Some(mut guard) = guard.take() {
        let _ = guard.kill();
        let _ = guard.wait();
    }
}

fn remove_fixture_root(root: &Path) -> io::Result<()> {
    match fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn diagnostic_operation_codes(output: &[u8]) -> Vec<u64> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| {
            line.split_once("performing daemon worker op: ")
                .and_then(|(_, value)| value.parse().ok())
        })
        .collect()
}

fn fixture_user() -> io::Result<String> {
    let output = Command::new("id").arg("-un").output()?;
    if !output.status.success() {
        return Err(io::Error::other("fixture user lookup failed"));
    }
    String::from_utf8(output.stdout)
        .map(|name| name.trim().to_owned())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "fixture user is not UTF-8"))
}

fn unique_suffix() -> u128 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    }
}
