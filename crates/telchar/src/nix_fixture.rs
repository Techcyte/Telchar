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
    socket_path: PathBuf,
}

pub struct NixFixture {
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
            return Err(io::Error::other("fixture setup failed"));
        }

        tracing::info!(
            event = "nix.fixture.setup.finished",
            state = "isolated",
            "Nix fixture setup finished"
        );
        Ok(Self {
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
        let user = fixture_user()?;
        if user == "root" {
            return Err(io::Error::other("fixture daemon requires a non-root client user"));
        }
        let trusted_users = match mode {
            TrustMode::Trusted => user,
            TrustMode::Untrusted => "root".to_owned(),
        };
        let config = format!(
            "{}\nallowed-users = *\nbuild-users-group =\nsandbox = false\ntrusted-users = {trusted_users}\n",
            fs::read_to_string(&self.config_path)?
        );
        let mut child = Command::new("nix-daemon")
            .envs(self.daemon_environment(config))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        for _ in 0..100 {
            if self.socket_path.exists() {
                tracing::info!(
                    event = "nix.fixture.daemon.started",
                    trust_mode = ?mode,
                    "Fixture daemon started"
                );
                return Ok(NixDaemon {
                    child,
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

    pub fn environment(&self) -> BTreeMap<&str, String> {
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
            ("NIX_DAEMON_SOCKET_PATH", self.socket_path.display().to_string()),
        ])
    }

    fn daemon_environment(&self, config: String) -> BTreeMap<&str, String> {
        let mut environment = self.environment();
        environment.insert("NIX_CONFIG", config);
        environment
    }

    pub fn cleanup(self) -> io::Result<()> {
        let lifecycle =
            tracing::info_span!("nix_fixture.cleanup", fixture = "isolated", client = "nix");
        let _entered = lifecycle.enter();
        tracing::info!(
            event = "nix.fixture.cleanup.started",
            "Nix fixture cleanup started"
        );
        fs::remove_dir_all(self.root)?;
        tracing::info!(
            event = "nix.fixture.cleanup.finished",
            "Nix fixture cleanup finished"
        );
        Ok(())
    }
}

impl NixDaemon {
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn store_url(&self) -> String {
        format!("unix://{}", self.socket_path.display())
    }

    pub fn trusted(&mut self) -> io::Result<bool> {
        let output = Command::new("nix")
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

    pub fn stop(&mut self) -> io::Result<()> {
        self.child.kill()?;
        self.child.wait()?;
        tracing::info!(
            event = "nix.fixture.daemon.stopped",
            "Fixture daemon stopped"
        );
        Ok(())
    }
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos()
}
