use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::config::NomadBackendConfig;

pub fn deterministic_job_name(config: &NomadBackendConfig, shared_build_key: &[u8]) -> String {
    let digest = Sha256::digest(shared_build_key);
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}-{suffix}", config.job_name_scope())
}

pub fn render_job(config: &NomadBackendConfig, shared_build_key: &[u8]) -> Value {
    let task = json!({
        "Name": "build",
        "Driver": config.driver(),
        "Config": Value::Object(config.driver_config().clone()),
        "Resources": {
            "CPU": config.resources().cpu_mhz(),
            "MemoryMB": config.resources().memory_mb(),
            "DiskMB": config.resources().disk_mb(),
        },
    });
    let mut group = Map::new();
    group.insert("Name".to_owned(), Value::String("build".to_owned()));
    group.insert("Count".to_owned(), Value::from(1));
    group.insert("Tasks".to_owned(), Value::Array(vec![task]));
    json!({
        "Job": {
            "ID": deterministic_job_name(config, shared_build_key),
            "Name": deterministic_job_name(config, shared_build_key),
            "Type": "batch",
            "Namespace": config.namespace(),
            "Datacenters": ["*"],
            "TaskGroups": [Value::Object(group)],
            "Meta": {
                "telchar_backend": config.target().name(),
                "telchar_system": config.target().system(),
            },
        }
    })
}
