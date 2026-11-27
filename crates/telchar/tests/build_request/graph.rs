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
        Default::default(),
    )
}

#[test]
fn configured_limits_control_graph_admission() {
    let root = std::env::temp_dir().join(format!("telchar-graph-config-{}", std::process::id()));
    std::fs::write(&root, "maximum_stored_build_nodes = 2\nmaximum_stored_build_depth = 2\nmaximum_stored_build_bytes = 1048576\n").unwrap();
    let config =
        telchar::service::config::ServiceConfig::load_with_environment(&root, |_| None).unwrap();
    std::fs::remove_file(root).unwrap();
    let limits = config.stored_build_limits();
    let load = |store: &mut Store, limits| {
        BuildRequest::load_stored_builds(
            Path::new(&path(0)),
            store,
            &mut Roots,
            &backends("x86_64-linux", &[]),
            limits,
        )
    };
    assert_eq!(load(&mut graph(2, false), limits).unwrap().len(), 2);
    let error = load(&mut graph(3, false), limits).unwrap_err();
    assert_eq!(
        telchar::build::stored_load_failure_phase(&error)
            .unwrap()
            .as_str(),
        "dependency-node-limit"
    );
    let error = load(
        &mut graph(3, true),
        telchar::build::StoredBuildLimits::new(10, 2, 1048576).unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        telchar::build::stored_load_failure_phase(&error)
            .unwrap()
            .as_str(),
        "dependency-depth-limit"
    );
    let mut store = graph(2, false);
    let bytes = store
        .0
        .values()
        .map(|nar| {
            telchar::store::nar::read_regular_nar(nar.as_slice(), 1048576)
                .unwrap()
                .len() as u64
        })
        .sum::<u64>();
    assert_eq!(
        load(
            &mut store,
            telchar::build::StoredBuildLimits::new(10, 2, bytes).unwrap()
        )
        .unwrap()
        .len(),
        2
    );
    let error = load(
        &mut store,
        telchar::build::StoredBuildLimits::new(10, 2, bytes - 1).unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        telchar::build::stored_load_failure_phase(&error)
            .unwrap()
            .as_str(),
        "derivation-byte-limit"
    );
}

#[derive(Clone)]
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn graph_summaries_report_consumption_and_limits() {
    const CHILD: &str = "TELCHAR_GRAPH_SUMMARY_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "graph::graph_summaries_report_consumption_and_limits",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "summary subprocess failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let writer = Capture(bytes.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let mut store = graph(3, true);
    let expected_bytes = store
        .0
        .values()
        .map(|nar| {
            telchar::store::nar::read_regular_nar(nar.as_slice(), 1048576)
                .unwrap()
                .len() as u64
        })
        .sum::<u64>();
    tracing::subscriber::with_default(subscriber, || {
        assert_eq!(load(&mut store).unwrap().len(), 3);
        let error = BuildRequest::load_stored_builds(
            Path::new(&path(0)),
            &mut store,
            &mut Roots,
            &backends("x86_64-linux", &[]),
            telchar::build::StoredBuildLimits::new(2, 128, 67108864).unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            telchar::build::stored_load_failure_phase(&error)
                .unwrap()
                .as_str(),
            "dependency-node-limit"
        );
    });
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let admitted = logs
        .lines()
        .find(|line| line.contains("event=\"gateway.stored_graph.admitted\""))
        .expect("successful admission summary");
    for field in [
        "nodes=3".to_owned(),
        "depth=3".to_owned(),
        format!("derivation_bytes={expected_bytes}"),
        "node_limit=4096".to_owned(),
        "depth_limit=128".to_owned(),
        "byte_limit=67108864".to_owned(),
    ] {
        assert!(admitted.contains(&field), "missing {field}: {admitted}");
    }
    let rejected = logs
        .lines()
        .find(|line| line.contains("event=\"gateway.stored_graph.failed\""))
        .unwrap_or_else(|| panic!("failed admission summary: {logs}"));
    for field in [
        "phase=\"dependency-node-limit\"",
        "nodes=2",
        "depth=2",
        "node_limit=2",
        "depth_limit=128",
        "byte_limit=67108864",
        "derivation_bytes=",
    ] {
        assert!(rejected.contains(field), "missing {field}: {rejected}");
    }
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
