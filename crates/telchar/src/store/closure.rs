//! Walks gateway-store references to compute the complete bounded admitted input closure.

use std::collections::{BTreeSet, VecDeque};
use std::io;

use crate::store::daemon::{GatewayStoreConnection, GatewayStoreEndpoint};

const MAXIMUM_CLOSURE_PATHS: usize = nix_worker_protocol::MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES;
const MAXIMUM_CLOSURE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosurePath {
    pub store_path: String,
    pub nar_hash: String,
    pub nar_size: u64,
    pub references: Vec<String>,
    pub deriver: Option<String>,
}

pub trait StoreClosureBackend: Send {
    fn input_closure(&mut self, roots: &[Vec<u8>]) -> io::Result<Vec<ClosurePath>>;
}

pub fn backend_from_environment() -> io::Result<Box<dyn StoreClosureBackend>> {
    let Some(value) = std::env::var_os("TELCHAR_GATEWAY_STORE_URI") else {
        return Ok(Box::new(UnavailableStoreClosureBackend));
    };
    let endpoint = GatewayStoreEndpoint::parse_os(&value).map_err(|_| query_error())?;
    Ok(Box::new(GatewayStoreClosureBackend::new(endpoint)))
}

pub struct UnavailableStoreClosureBackend;

impl StoreClosureBackend for UnavailableStoreClosureBackend {
    fn input_closure(&mut self, roots: &[Vec<u8>]) -> io::Result<Vec<ClosurePath>> {
        if roots.is_empty() {
            Ok(Vec::new())
        } else {
            Err(query_error())
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
        let mut connection =
            GatewayStoreConnection::connect(&self.endpoint).map_err(|_| query_error())?;
        compute_input_closure(&mut connection, roots)
    }
}

struct ClosurePathInfo {
    nar_hash: String,
    nar_size: u64,
    references: Vec<Vec<u8>>,
    deriver: Option<Vec<u8>>,
}

trait PathInfoQuery {
    fn query_path(&mut self, path: &[u8]) -> io::Result<Option<ClosurePathInfo>>;
}

impl PathInfoQuery for GatewayStoreConnection {
    fn query_path(&mut self, path: &[u8]) -> io::Result<Option<ClosurePathInfo>> {
        self.query_path_info(path).map(|info| {
            info.map(|info| ClosurePathInfo {
                nar_hash: info.nar_hash_hex().to_owned(),
                nar_size: info.nar_size(),
                references: info.references().to_vec(),
                deriver: info.deriver().map(ToOwned::to_owned),
            })
        })
    }
}

fn compute_input_closure(
    store: &mut impl PathInfoQuery,
    roots: &[Vec<u8>],
) -> io::Result<Vec<ClosurePath>> {
    if roots.len() > MAXIMUM_CLOSURE_PATHS {
        return Err(query_error());
    }
    let mut pending = VecDeque::new();
    let mut discovered = BTreeSet::new();
    let mut retained_bytes = 0_usize;
    let mut metadata = std::collections::BTreeMap::new();
    for root in roots {
        add_path(root, &mut pending, &mut discovered, &mut retained_bytes)?;
    }

    while let Some(path) = pending.pop_front() {
        let info = store
            .query_path(&path)
            .map_err(|_| query_error())?
            .ok_or_else(query_error)?;
        if info.nar_size == 0 || metadata.insert(path.clone(), info).is_some() {
            return Err(query_error());
        }
        for reference in &metadata.get(&path).ok_or_else(query_error)?.references {
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
            let info = metadata.remove(&path).ok_or_else(query_error)?;
            Ok(ClosurePath {
                store_path: String::from_utf8(path).map_err(|_| query_error())?,
                nar_hash: info.nar_hash,
                nar_size: info.nar_size,
                references: info
                    .references
                    .into_iter()
                    .map(|reference| String::from_utf8(reference).map_err(|_| query_error()))
                    .collect::<io::Result<Vec<_>>>()?,
                deriver: info
                    .deriver
                    .map(|deriver| String::from_utf8(deriver).map_err(|_| query_error()))
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
        return Err(query_error());
    }
    *retained_bytes = retained_bytes
        .checked_add(path.len())
        .filter(|bytes| *bytes <= MAXIMUM_CLOSURE_BYTES)
        .ok_or_else(query_error)?;
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
        return Err(query_error());
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
        return Err(query_error());
    }
    Ok(())
}

fn query_error() -> io::Error {
    io::Error::other("input closure query failed")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const ROOT: &[u8] = b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root";
    const LEFT: &[u8] = b"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-left";
    const RIGHT: &[u8] = b"/nix/store/cccccccccccccccccccccccccccccccc-right";
    const LEAF: &[u8] = b"/nix/store/dddddddddddddddddddddddddddddddd-leaf";

    struct Store {
        paths: BTreeMap<Vec<u8>, Vec<Vec<u8>>>,
        queries: Vec<Vec<u8>>,
    }

    impl PathInfoQuery for Store {
        fn query_path(&mut self, path: &[u8]) -> io::Result<Option<ClosurePathInfo>> {
            self.queries.push(path.to_vec());
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
                }))
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
            queries: Vec::new(),
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
                })
                .collect::<Vec<_>>()
        );
        store.queries.sort();
        store.queries.dedup();
        assert_eq!(store.queries.len(), 4);
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
    fn missing_root_or_reference_fails_closed() {
        let mut missing_root = store();
        assert!(compute_input_closure(
            &mut missing_root,
            &[b"/nix/store/00000000000000000000000000000000-missing".to_vec()]
        )
        .is_err());

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
