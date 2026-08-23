//! Walks gateway-store references to compute the complete bounded admitted input closure.

use std::collections::{BTreeSet, VecDeque};
use std::io;

use crate::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

const MAXIMUM_CLOSURE_PATHS: usize = nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES;
const MAXIMUM_CLOSURE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosureFailurePhase {
    Connect,
    ValidatePath,
    QueryPath,
    EnsurePath,
    MissingPath,
    Metadata,
}

impl ClosureFailurePhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::ValidatePath => "validate-path",
            Self::QueryPath => "query-path",
            Self::EnsurePath => "ensure-path",
            Self::MissingPath => "missing-path",
            Self::Metadata => "metadata",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosurePathKind {
    Root,
    Reference,
}

impl ClosurePathKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Reference => "reference",
        }
    }
}

#[derive(Debug)]
struct ClosureFailure {
    phase: ClosureFailurePhase,
    path: Option<String>,
    path_kind: Option<ClosurePathKind>,
}

impl std::fmt::Display for ClosureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("input closure query failed")
    }
}

impl std::error::Error for ClosureFailure {}

pub fn failure_phase(error: &io::Error) -> Option<ClosureFailurePhase> {
    error
        .get_ref()
        .and_then(|error| error.downcast_ref::<ClosureFailure>())
        .map(|failure| failure.phase)
}

pub fn failure_path(error: &io::Error) -> Option<(&str, ClosurePathKind)> {
    error
        .get_ref()
        .and_then(|error| error.downcast_ref::<ClosureFailure>())
        .and_then(|failure| Some((failure.path.as_deref()?, failure.path_kind?)))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosurePath {
    pub store_path: String,
    pub nar_hash: String,
    pub nar_size: u64,
    pub references: Vec<String>,
    pub deriver: Option<String>,
    pub content_address: Option<String>,
}

pub trait StoreClosureBackend: Send {
    fn input_closure(&mut self, roots: &[Vec<u8>]) -> io::Result<Vec<ClosurePath>>;
}

pub fn backend_from_environment() -> io::Result<Box<dyn StoreClosureBackend>> {
    let Some(value) = std::env::var_os("TELCHAR_GATEWAY_STORE_URI") else {
        return Ok(Box::new(UnavailableStoreClosureBackend));
    };
    let endpoint = GatewayStoreEndpoint::parse_os(&value)
        .map_err(|_| query_error(ClosureFailurePhase::Connect))?;
    Ok(Box::new(GatewayStoreClosureBackend::new(endpoint)))
}

pub struct UnavailableStoreClosureBackend;

impl StoreClosureBackend for UnavailableStoreClosureBackend {
    fn input_closure(&mut self, roots: &[Vec<u8>]) -> io::Result<Vec<ClosurePath>> {
        if roots.is_empty() {
            Ok(Vec::new())
        } else {
            Err(query_error(ClosureFailurePhase::Connect))
        }
    }
}

pub struct GatewayStoreClosureBackend {
    endpoint: GatewayStoreEndpoint,
}

impl GatewayStoreClosureBackend {
    pub fn new(endpoint: GatewayStoreEndpoint) -> Self {
        Self { endpoint }
    }
}

impl StoreClosureBackend for GatewayStoreClosureBackend {
    fn input_closure(&mut self, roots: &[Vec<u8>]) -> io::Result<Vec<ClosurePath>> {
        if roots.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = GatewayStoreConnection::connect(&self.endpoint)
            .map_err(|_| query_error(ClosureFailurePhase::Connect))?;
        compute_input_closure(&mut connection, roots)
    }
}

struct ClosurePathInfo {
    nar_hash: String,
    nar_size: u64,
    references: Vec<Vec<u8>>,
    deriver: Option<Vec<u8>>,
    content_address: Option<Vec<u8>>,
}

trait PathInfoQuery {
    fn query_path(&mut self, path: &[u8]) -> io::Result<Option<ClosurePathInfo>>;
    fn ensure_path(&mut self, path: &[u8]) -> io::Result<()>;
}

impl PathInfoQuery for GatewayStoreConnection {
    fn query_path(&mut self, path: &[u8]) -> io::Result<Option<ClosurePathInfo>> {
        self.query_path_info(path).map(|info| {
            info.map(|info| ClosurePathInfo {
                nar_hash: info.nar_hash_hex().to_owned(),
                nar_size: info.nar_size(),
                references: info.references().to_vec(),
                deriver: info.deriver().map(ToOwned::to_owned),
                content_address: info.content_address().map(ToOwned::to_owned),
            })
        })
    }

    fn ensure_path(&mut self, path: &[u8]) -> io::Result<()> {
        GatewayStoreConnection::ensure_path(self, path)
    }
}

fn compute_input_closure(
    store: &mut impl PathInfoQuery,
    roots: &[Vec<u8>],
) -> io::Result<Vec<ClosurePath>> {
    if roots.len() > MAXIMUM_CLOSURE_PATHS {
        return Err(query_error(ClosureFailurePhase::ValidatePath));
    }
    let root_paths = roots.iter().map(Vec::as_slice).collect::<BTreeSet<_>>();
    let mut pending = VecDeque::new();
    let mut discovered = BTreeSet::new();
    let mut retained_bytes = 0_usize;
    let mut metadata = std::collections::BTreeMap::new();
    for root in roots {
        add_path(root, &mut pending, &mut discovered, &mut retained_bytes)?;
    }

    while let Some(path) = pending.pop_front() {
        let path_kind = if root_paths.contains(path.as_slice()) {
            ClosurePathKind::Root
        } else {
            ClosurePathKind::Reference
        };
        let mut info = store
            .query_path(&path)
            .map_err(|_| path_error(ClosureFailurePhase::QueryPath, &path, path_kind))?;
        if info.is_none() {
            store
                .ensure_path(&path)
                .map_err(|_| path_error(ClosureFailurePhase::EnsurePath, &path, path_kind))?;
            info = store
                .query_path(&path)
                .map_err(|_| path_error(ClosureFailurePhase::QueryPath, &path, path_kind))?;
        }
        let info =
            info.ok_or_else(|| path_error(ClosureFailurePhase::MissingPath, &path, path_kind))?;
        if info.nar_size == 0 || metadata.insert(path.clone(), info).is_some() {
            return Err(query_error(ClosureFailurePhase::Metadata));
        }
        for reference in &metadata
            .get(&path)
            .ok_or_else(|| query_error(ClosureFailurePhase::Metadata))?
            .references
        {
            add_path(
                reference,
                &mut pending,
                &mut discovered,
                &mut retained_bytes,
            )?;
        }
    }

    discovered
        .into_iter()
        .map(|path| {
            let info = metadata
                .remove(&path)
                .ok_or_else(|| query_error(ClosureFailurePhase::Metadata))?;
            Ok(ClosurePath {
                store_path: String::from_utf8(path)
                    .map_err(|_| query_error(ClosureFailurePhase::Metadata))?,
                nar_hash: info.nar_hash,
                nar_size: info.nar_size,
                references: info
                    .references
                    .into_iter()
                    .map(|reference| {
                        String::from_utf8(reference)
                            .map_err(|_| query_error(ClosureFailurePhase::Metadata))
                    })
                    .collect::<io::Result<Vec<_>>>()?,
                deriver: info
                    .deriver
                    .map(|deriver| {
                        String::from_utf8(deriver)
                            .map_err(|_| query_error(ClosureFailurePhase::Metadata))
                    })
                    .transpose()?,
                content_address: info
                    .content_address
                    .map(|address| {
                        String::from_utf8(address)
                            .map_err(|_| query_error(ClosureFailurePhase::Metadata))
                    })
                    .transpose()?,
            })
        })
        .collect()
}

fn add_path(
    path: &[u8],
    pending: &mut VecDeque<Vec<u8>>,
    discovered: &mut BTreeSet<Vec<u8>>,
    retained_bytes: &mut usize,
) -> io::Result<()> {
    validate_store_path(path)?;
    if discovered.contains(path) {
        return Ok(());
    }
    if discovered.len() >= MAXIMUM_CLOSURE_PATHS {
        return Err(query_error(ClosureFailurePhase::ValidatePath));
    }
    *retained_bytes = retained_bytes
        .checked_add(path.len())
        .filter(|bytes| *bytes <= MAXIMUM_CLOSURE_BYTES)
        .ok_or_else(|| query_error(ClosureFailurePhase::ValidatePath))?;
    let path = path.to_vec();
    discovered.insert(path.clone());
    pending.push_back(path);
    Ok(())
}

fn validate_store_path(path: &[u8]) -> io::Result<()> {
    const STORE_DIRECTORY: &[u8] = b"/nix/store/";
    const HASH_LENGTH: usize = 32;
    const HASH_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

    let Some(base) = path.strip_prefix(STORE_DIRECTORY) else {
        return Err(query_error(ClosureFailurePhase::ValidatePath));
    };
    if path.len() > nix_worker_protocol::MAXIMUM_WORKER_STORE_PATH_BYTES
        || base.len() <= HASH_LENGTH + 1
        || base.contains(&b'/')
        || base[HASH_LENGTH] != b'-'
        || !base[..HASH_LENGTH]
            .iter()
            .all(|byte| HASH_ALPHABET.contains(byte))
        || !base[HASH_LENGTH + 1..].iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
        })
    {
        return Err(query_error(ClosureFailurePhase::ValidatePath));
    }
    Ok(())
}

fn query_error(phase: ClosureFailurePhase) -> io::Error {
    io::Error::other(ClosureFailure {
        phase,
        path: None,
        path_kind: None,
    })
}

fn path_error(phase: ClosureFailurePhase, path: &[u8], path_kind: ClosurePathKind) -> io::Error {
    io::Error::other(ClosureFailure {
        phase,
        path: std::str::from_utf8(path).ok().map(str::to_owned),
        path_kind: Some(path_kind),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const ROOT: &[u8] = b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root";
    const ROOT_STR: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root";
    const LEFT: &[u8] = b"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-left";
    const RIGHT: &[u8] = b"/nix/store/cccccccccccccccccccccccccccccccc-right";
    const LEAF: &[u8] = b"/nix/store/dddddddddddddddddddddddddddddddd-leaf";

    struct Store {
        paths: BTreeMap<Vec<u8>, Vec<Vec<u8>>>,
        substitutable: BTreeMap<Vec<u8>, Vec<Vec<u8>>>,
        queries: Vec<Vec<u8>>,
        ensured: Vec<Vec<u8>>,
        query_failure: bool,
        ensure_failure: bool,
    }

    impl PathInfoQuery for Store {
        fn query_path(&mut self, path: &[u8]) -> io::Result<Option<ClosurePathInfo>> {
            self.queries.push(path.to_vec());
            if self.query_failure {
                return Err(io::Error::other("daemon query payload must stay hidden"));
            }
            Ok(self
                .paths
                .get(path)
                .cloned()
                .map(|references| ClosurePathInfo {
                    nar_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .to_owned(),
                    nar_size: 1,
                    references,
                    deriver: None,
                    content_address: None,
                }))
        }

        fn ensure_path(&mut self, path: &[u8]) -> io::Result<()> {
            self.ensured.push(path.to_vec());
            if self.ensure_failure {
                return Err(io::Error::other("daemon ensure payload must stay hidden"));
            }
            if let Some(references) = self.substitutable.remove(path) {
                self.paths.insert(path.to_vec(), references);
                Ok(())
            } else {
                Err(query_error(ClosureFailurePhase::EnsurePath))
            }
        }
    }

    fn store() -> Store {
        Store {
            paths: BTreeMap::from([
                (ROOT.to_vec(), vec![RIGHT.to_vec(), LEFT.to_vec()]),
                (LEFT.to_vec(), vec![LEAF.to_vec()]),
                (RIGHT.to_vec(), vec![LEAF.to_vec()]),
                (LEAF.to_vec(), Vec::new()),
            ]),
            substitutable: BTreeMap::new(),
            queries: Vec::new(),
            ensured: Vec::new(),
            query_failure: false,
            ensure_failure: false,
        }
    }

    #[test]
    fn computes_complete_reference_closure_once_in_deterministic_order() {
        let mut store = store();

        let closure = compute_input_closure(&mut store, &[ROOT.to_vec(), LEFT.to_vec()]).unwrap();

        assert_eq!(
            closure,
            [ROOT, LEFT, RIGHT, LEAF]
                .into_iter()
                .map(|path| ClosurePath {
                    store_path: String::from_utf8(path.to_vec()).unwrap(),
                    nar_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .to_owned(),
                    nar_size: 1,
                    references: store
                        .paths
                        .get(path)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|reference| String::from_utf8(reference).unwrap())
                        .collect(),
                    deriver: None,
                    content_address: None,
                })
                .collect::<Vec<_>>()
        );
        store.queries.sort();
        store.queries.dedup();
        assert_eq!(store.queries.len(), 4);
    }

    #[test]
    fn substitutes_missing_root_before_walking_references() {
        let mut store = store();
        store.paths.remove(ROOT);
        store
            .substitutable
            .insert(ROOT.to_vec(), vec![LEFT.to_vec(), RIGHT.to_vec()]);

        let closure = compute_input_closure(&mut store, &[ROOT.to_vec()]).unwrap();

        assert_eq!(store.ensured, vec![ROOT.to_vec()]);
        assert_eq!(closure.len(), 4);
    }

    #[test]
    fn empty_roots_do_not_query_or_connect() {
        let mut store = store();
        assert_eq!(
            compute_input_closure(&mut store, &[]).unwrap(),
            Vec::<ClosurePath>::new()
        );
        assert!(store.queries.is_empty());
    }

    #[test]
    fn reports_bounded_query_and_ensure_failure_phases() {
        let mut query_failure = store();
        query_failure.query_failure = true;
        let error = compute_input_closure(&mut query_failure, &[ROOT.to_vec()]).unwrap_err();
        assert_eq!(failure_phase(&error), Some(ClosureFailurePhase::QueryPath));
        assert!(!error.to_string().contains("daemon query payload"));

        let mut ensure_failure = store();
        ensure_failure.paths.remove(ROOT);
        ensure_failure.ensure_failure = true;
        let error = compute_input_closure(&mut ensure_failure, &[ROOT.to_vec()]).unwrap_err();
        assert_eq!(failure_phase(&error), Some(ClosureFailurePhase::EnsurePath));
        assert_eq!(
            failure_path(&error),
            Some((ROOT_STR, ClosurePathKind::Root))
        );
        assert!(!error.to_string().contains("daemon ensure payload"));
    }

    #[test]
    fn reports_reference_path_context_without_daemon_payload() {
        let mut reference_failure = store();
        reference_failure.paths.remove(LEAF);
        reference_failure.ensure_failure = true;

        let error = compute_input_closure(&mut reference_failure, &[ROOT.to_vec()]).unwrap_err();

        assert_eq!(failure_phase(&error), Some(ClosureFailurePhase::EnsurePath));
        assert_eq!(
            failure_path(&error),
            Some((
                "/nix/store/dddddddddddddddddddddddddddddddd-leaf",
                ClosurePathKind::Reference,
            ))
        );
        assert!(!error.to_string().contains("daemon ensure payload"));
    }

    #[test]
    fn reports_missing_path_after_ensure() {
        let mut missing = store();
        missing.paths.remove(ROOT);

        let error = compute_input_closure(&mut missing, &[ROOT.to_vec()]).unwrap_err();

        assert_eq!(failure_phase(&error), Some(ClosureFailurePhase::EnsurePath));
    }

    #[test]
    fn missing_root_or_reference_fails_closed() {
        let mut missing_root = store();
        assert!(
            compute_input_closure(
                &mut missing_root,
                &[b"/nix/store/00000000000000000000000000000000-missing".to_vec()]
            )
            .is_err()
        );

        let mut missing_reference = store();
        missing_reference.paths.remove(LEAF);
        assert!(compute_input_closure(&mut missing_reference, &[ROOT.to_vec()]).is_err());
    }

    #[test]
    fn cycles_and_duplicate_references_terminate_without_duplicate_results() {
        let mut store = store();
        store
            .paths
            .insert(LEAF.to_vec(), vec![ROOT.to_vec(), ROOT.to_vec()]);

        let closure = compute_input_closure(&mut store, &[ROOT.to_vec()]).unwrap();

        assert_eq!(closure.len(), 4);
        assert_eq!(store.queries.len(), 4);
    }

    #[test]
    fn malformed_paths_and_path_count_overflow_fail_before_queries() {
        for path in [
            b"relative".as_slice(),
            b"/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-invalid".as_slice(),
            b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a/nested".as_slice(),
            b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a!".as_slice(),
        ] {
            let mut store = store();
            assert!(compute_input_closure(&mut store, &[path.to_vec()]).is_err());
            assert!(store.queries.is_empty());
        }

        let mut store = store();
        let roots = vec![ROOT.to_vec(); MAXIMUM_CLOSURE_PATHS + 1];
        assert!(compute_input_closure(&mut store, &roots).is_err());
        assert!(store.queries.is_empty());
    }
}
