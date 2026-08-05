use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct NixFixture {
    root: PathBuf,
    config_path: PathBuf,
    private_key_path: PathBuf,
    public_key_path: PathBuf,
    state_dir: PathBuf,
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

    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    pub fn environment(&self) -> BTreeMap<&str, String> {
        BTreeMap::from([
            (
                "NIX_CONFIG",
                fs::read_to_string(&self.config_path).expect("fixture configuration exists"),
            ),
            ("TMPDIR", self.temp_dir.display().to_string()),
        ])
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

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_nanos()
}
