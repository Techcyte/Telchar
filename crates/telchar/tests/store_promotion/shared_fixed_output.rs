//! Real daemon contracts for one fixed-output path uploaded with different derivers.
//!
//! A fixed-output path is the same on every platform, but each platform
//! instantiates its own derivation for it. When clients on two platforms build
//! the same source, both upload that one path, each naming its own derivation
//! as the deriver. The gateway store keeps whichever registration arrives first.
//!
//! Run under tests/canonical-store.sh: no host-store registration or builders.

use std::sync::{Arc, Barrier};

use telchar::store::promotion::GatewayStorePromotionBackend;

use super::text_content_address::{backend, import, nix};
use super::*;

fn text(output: Vec<u8>) -> String {
    String::from_utf8(output).unwrap().trim().to_owned()
}

/// A fixed-output derivation of `source`; `platform` changes the derivation,
/// and so its path, but not its output path.
fn fixed_output_expression(source: &str, platform: &str) -> String {
    format!(
        "derivation {{ name = \"{source}\"; system = builtins.currentSystem; \
         builder = \"/bin/sh\"; \
         args = [ \"-c\" \"printf %s {source} > \\\"$out\\\" # built for {platform}\" ]; \
         outputHashMode = \"flat\"; outputHashAlgo = \"sha256\"; \
         outputHash = builtins.hashString \"sha256\" \"{source}\"; }}"
    )
}

fn instantiate(source: &str, platform: &str) -> PathBuf {
    let expression = fixed_output_expression(source, platform);
    PathBuf::from(text(nix("nix-instantiate", &["--expr", &expression])))
}

fn output_of(derivation: &Path) -> PathBuf {
    PathBuf::from(text(nix(
        "nix-store",
        &["--query", "--outputs", derivation.to_str().unwrap()],
    )))
}

/// One fixed-output path, registered in the gateway store by the first
/// platform's derivation, as a client on that platform left it.
struct SharedFixedOutput {
    first_deriver: PathBuf,
    second_deriver: PathBuf,
    registered: RegisteredPathInfo,
    nar: Vec<u8>,
}

impl SharedFixedOutput {
    fn registered(source: &str) -> Self {
        let first_deriver = instantiate(source, "first-platform");
        let second_deriver = instantiate(source, "second-platform");
        let path = output_of(&first_deriver);
        assert_eq!(
            output_of(&second_deriver),
            path,
            "derivations share one output"
        );
        assert_ne!(
            first_deriver, second_deriver,
            "each platform has its own derivation"
        );
        nix("nix-store", &["--realise", first_deriver.to_str().unwrap()]);
        let registered = backend().query_path_info(&path).unwrap();
        assert_eq!(registered.deriver.as_ref(), Some(&first_deriver));
        assert!(
            registered
                .content_address
                .as_deref()
                .is_some_and(|address| address.starts_with("fixed:")),
            "{:?}",
            registered.content_address
        );
        let exported = nix("nix-store", &["--export", path.to_str().unwrap()]);
        Self {
            first_deriver,
            second_deriver,
            registered,
            nar: raw_nar_from_export(&exported),
        }
    }

    /// What a client uploading this path declares when `deriver` built it.
    fn declared(&self, deriver: &Path) -> DeclaredPathInfo {
        DeclaredPathInfo {
            path: self.registered.path.clone(),
            nar_hash: self.registered.nar_hash,
            nar_size: self.registered.nar_size,
            references: self.registered.references.clone(),
            deriver: Some(deriver.to_path_buf()),
            content_address: self.registered.content_address.clone(),
            signatures: Vec::new(),
            ultimate: false,
        }
    }
}

/// Holds each upload just before promotion until every upload reaches that
/// point: two clients that each found the path missing, uploading at once.
struct OverlappingBackend {
    inner: GatewayStorePromotionBackend,
    barrier: Arc<Barrier>,
}

impl StorePromotionBackend for OverlappingBackend {
    fn store_uri(&self) -> &str {
        self.inner.store_uri()
    }

    fn is_valid_path(&mut self, path: &Path) -> io::Result<bool> {
        self.inner.is_valid_path(path)
    }

    fn before_promote(&mut self, request: &PromotionRequest) -> io::Result<()> {
        self.barrier.wait();
        self.inner.before_promote(request)
    }

    fn promote(&mut self, request: &PromotionRequest) -> io::Result<()> {
        self.inner.promote(request)
    }

    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
        self.inner.query_path_info(path)
    }
}

#[test]
fn reupload_naming_the_registered_deriver_imports() {
    let shared = SharedFixedOutput::registered("telchar-shared-source-same-deriver");

    import(&shared.declared(&shared.first_deriver), &shared.nar)
        .expect("re-upload naming the registered deriver imports");
}

#[test]
fn upload_from_another_platform_imports_after_the_path_is_registered() {
    let shared = SharedFixedOutput::registered("telchar-shared-source-other-platform");

    let result = import(&shared.declared(&shared.second_deriver), &shared.nar);

    result.expect(
        "a fixed-output path already valid with the declared NAR hash must import, \
         whichever platform's derivation registered it",
    );
    assert_eq!(
        backend().query_path_info(&shared.registered.path).unwrap(),
        shared.registered
    );
}

#[test]
fn overlapping_uploads_from_two_platforms_both_promote() {
    let shared = SharedFixedOutput::registered("telchar-shared-source-overlap");
    let path = shared.registered.path.clone();
    nix("nix-store", &["--delete", path.to_str().unwrap()]);
    assert!(!backend().is_valid_path(&path).unwrap());

    let barrier = Arc::new(Barrier::new(2));
    let uploads = [&shared.first_deriver, &shared.second_deriver].map(|deriver| {
        let mut overlapping = OverlappingBackend {
            inner: backend(),
            barrier: Arc::clone(&barrier),
        };
        let declared = shared.declared(deriver);
        let nar = shared.nar.clone();
        std::thread::spawn(move || {
            let staging = TestDirectory::create();
            validate_and_promote_nar(
                Cursor::new(nar),
                staging.path(),
                Path::new(STORE_DIRECTORY),
                &declared,
                &mut overlapping,
            )
            .map(|_| ())
            .map_err(|error| format!("{}: {error}", declared.deriver.unwrap().display()))
        })
    });
    let failures: Vec<String> = uploads
        .into_iter()
        .filter_map(|upload| upload.join().unwrap().err())
        .collect();

    assert!(
        backend().is_valid_path(&path).unwrap(),
        "no upload registered the path"
    );
    assert!(
        failures.is_empty(),
        "overlapping uploads failed: {failures:#?}"
    );
}
