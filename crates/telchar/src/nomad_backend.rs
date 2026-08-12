use std::fs;
use std::io::{self, Read};

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Certificate, Identity};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::config::NomadBackendConfig;

const MAXIMUM_NOMAD_RESPONSE_BYTES: u64 = 1024 * 1024;

pub struct NomadClient {
    config: NomadBackendConfig,
    client: Client,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NomadSubmission {
    job_id: String,
    evaluation_id: String,
}

impl NomadSubmission {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn evaluation_id(&self) -> &str {
        &self.evaluation_id
    }
}

#[derive(Deserialize)]
struct SubmissionResponse {
    #[serde(rename = "EvalID")]
    eval_id: String,
}

impl NomadClient {
    pub fn new(config: NomadBackendConfig) -> io::Result<Self> {
        let mut headers = HeaderMap::new();
        if let Some(path) = config.token_file() {
            let token = fs::read_to_string(path)
                .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
            let token = token.trim();
            if token.is_empty() {
                return Err(io::Error::other("Nomad client configuration failed"));
            }
            headers.insert(
                "X-Nomad-Token",
                HeaderValue::from_str(token)
                    .map_err(|_| io::Error::other("Nomad client configuration failed"))?,
            );
        }
        let mut builder = Client::builder()
            .default_headers(headers)
            .timeout(config.runtime_limit());
        if let Some(path) = config.ca_certificate_file() {
            let pem = fs::read(path)
                .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
            let certificates = Certificate::from_pem_bundle(&pem)
                .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
            if certificates.is_empty() {
                return Err(io::Error::other("Nomad client configuration failed"));
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        if let (Some(certificate_path), Some(key_path)) =
            (config.client_certificate_file(), config.client_key_file())
        {
            let mut pem = fs::read(certificate_path)
                .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
            pem.extend_from_slice(
                &fs::read(key_path)
                    .map_err(|_| io::Error::other("Nomad client configuration failed"))?,
            );
            builder = builder.identity(
                Identity::from_pem(&pem)
                    .map_err(|_| io::Error::other("Nomad client configuration failed"))?,
            );
        }
        let client = builder
            .build()
            .map_err(|_| io::Error::other("Nomad client configuration failed"))?;
        Ok(Self { config, client })
    }

    pub fn submit(&self, shared_build_key: &[u8]) -> io::Result<NomadSubmission> {
        let job_id = deterministic_job_name(&self.config, shared_build_key);
        let response = self
            .client
            .post(format!("{}/v1/jobs", self.config.endpoint()))
            .query(&[("namespace", self.config.namespace())])
            .json(&render_job(&self.config, shared_build_key))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|_| io::Error::other("Nomad job submission failed"))?;
        if response
            .content_length()
            .is_some_and(|length| length > MAXIMUM_NOMAD_RESPONSE_BYTES)
        {
            return Err(io::Error::other("Nomad job submission failed"));
        }
        let parsed: SubmissionResponse =
            serde_json::from_reader(response.take(MAXIMUM_NOMAD_RESPONSE_BYTES + 1))
                .map_err(|_| io::Error::other("Nomad job submission failed"))?;
        if parsed.eval_id.is_empty() || parsed.eval_id.len() > 256 {
            return Err(io::Error::other("Nomad job submission failed"));
        }
        Ok(NomadSubmission {
            job_id,
            evaluation_id: parsed.eval_id,
        })
    }
}

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
