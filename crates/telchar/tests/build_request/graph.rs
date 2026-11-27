use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use telchar::store::export::{StoreExportBackend, StoreExportRequest, TemporaryRoots};
use telchar::store::promotion::RegisteredPathInfo;

struct Store(BTreeMap<PathBuf, Vec<u8>>);

impl StoreExportBackend for Store {
    fn store_uri(&self) -> &str {
        "fixture"
    }

    fn query_path_info(&mut self, path: &Path) -> io::Result<RegisteredPathInfo> {
        let nar = self
            .0
            .get(path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Ok(RegisteredPathInfo {
            path: path.to_path_buf(),
            nar_hash: sha2::Sha256::digest(nar).into(),
            nar_size: nar.len() as u64,
            references: Vec::new(),
            deriver: None,
            content_address: None,
        })
    }

    fn export_nar(
        &mut self,
        request: &StoreExportRequest,
        _: u64,
        sink: &mut dyn io::Write,
    ) -> io::Result<()> {
        sink.write_all(self.0.get(&request.path).unwrap())
    }
}

struct Roots;
impl TemporaryRoots for Roots {
    fn retain(&mut self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

fn path(index: usize) -> String {
    format!("/nix/store/11111111111111111111111111111111-node-{index}.drv")
}

fn graph(count: usize, chain: bool) -> Store {
    let mut paths = BTreeMap::new();
    for index in 0..count {
        let inputs = if chain {
            if index + 1 < count {
                vec![index + 1]
            } else {
                Vec::new()
            }
        } else if index == 0 {
            (1..count).collect()
        } else {
            Vec::new()
        };
        let inputs = inputs
            .iter()
            .map(|index| format!("(\"{}\",[\"out\"])", path(*index)))
            .collect::<Vec<_>>()
            .join(",");
        let output = format!("/nix/store/11111111111111111111111111111111-node-{index}");
        let contents = format!(
            "Derive([(\"out\",\"{output}\",\"\",\"\")],[{inputs}],[],\"x86_64-linux\",\"/bin/sh\",[],[(\"out\",\"{output}\")])"
        );
        paths.insert(PathBuf::from(path(index)), regular_nar(contents.as_bytes()));
    }
    Store(paths)
}

fn load(store: &mut Store) -> io::Result<Vec<BuildRequest>> {
    BuildRequest::load_stored_builds(
        Path::new(&path(0)),
        store,
        &mut Roots,
        &backends("x86_64-linux", &[]),
    )
}

#[test]
fn admits_wide_graph_through_node_budget() {
    let builds = load(&mut graph(4096, false)).expect("graph within node budget");
    assert_eq!(builds.len(), 4096);
    assert_eq!(builds.last().unwrap().derivation_path(), path(0).as_bytes());
}

#[test]
fn reports_node_budget_separately_from_parse_failure() {
    let error = load(&mut graph(4097, false)).unwrap_err();
    assert_eq!(
        telchar::build::stored_load_failure_phase(&error)
            .unwrap()
            .as_str(),
        "dependency-node-limit"
    );
}

#[test]
fn reports_depth_budget_separately_from_parse_failure() {
    assert_eq!(load(&mut graph(128, true)).unwrap().len(), 128);
    let error = load(&mut graph(129, true)).unwrap_err();
    assert_eq!(
        telchar::build::stored_load_failure_phase(&error)
            .unwrap()
            .as_str(),
        "dependency-depth-limit"
    );
}
