//! Conservative peak compute quotes from the public AWS regional catalog.

use super::ProviderError;
use serde_json::Value;
use std::io::Read;
use std::time::{Duration, SystemTime};

const MAXIMUM_CATALOG_BYTES: u64 = 16 * 1024 * 1024;
const QUOTE_LIFETIME: Duration = Duration::from_secs(3600);

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct ComputePrice {
    picodollars_per_second: i64,
    reference: String,
}

impl ComputePrice {
    pub fn from_catalog(
        catalog: &Value,
        region: &str,
        minimum_memory_mib: i32,
    ) -> Result<Self, ProviderError> {
        let (memory, cpu) = match minimum_memory_mib {
            512 => (2i64, 1i64),
            1024 => (4, 2),
            2048 => (8, 4),
            4096 => (16, 8),
            8192 => (32, 16),
            _ => return Err(ProviderError),
        };
        if catalog["offerCode"].as_str() != Some("AWSLambda") {
            return Err(ProviderError);
        }
        let version = catalog["version"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(ProviderError)?;
        let cpu_rate = rate(
            catalog,
            region,
            "AWS-Lambda-MicroVM-vCPU-Second-ARM",
            "vCPU-Seconds",
        )?;
        let memory_rate = rate(
            catalog,
            region,
            "AWS-Lambda-MicroVM-Memory-GB-Second-ARM",
            "GB-Seconds",
        )?;
        let amount = cpu_rate
            .checked_mul(cpu)
            .and_then(|v| {
                memory_rate
                    .checked_mul(memory)
                    .and_then(|m| v.checked_add(m))
            })
            .filter(|v| *v > 0)
            .ok_or(ProviderError)?;
        Ok(Self {
            picodollars_per_second: amount,
            reference: format!("AWSLambda/{region}/{version}/arm64/peak-{memory}GB-{cpu}vCPU"),
        })
    }

    pub fn picodollars_per_second(&self) -> i64 {
        self.picodollars_per_second
    }

    pub fn reference(&self) -> &str {
        &self.reference
    }
}

fn rate(catalog: &Value, region: &str, group: &str, unit: &str) -> Result<i64, ProviderError> {
    let products = catalog["products"].as_object().ok_or(ProviderError)?;
    let mut matches = products.iter().filter(|(_, product)| {
        let a = &product["attributes"];
        a["servicecode"].as_str() == Some("AWSLambda")
            && a["regionCode"].as_str() == Some(region)
            && a["group"].as_str() == Some(group)
    });
    let (sku, _) = matches.next().ok_or(ProviderError)?;
    if matches.next().is_some() {
        return Err(ProviderError);
    }
    let terms = catalog["terms"]["OnDemand"][sku]
        .as_object()
        .filter(|v| v.len() == 1)
        .ok_or(ProviderError)?;
    let term = terms.values().next().ok_or(ProviderError)?;
    let dimensions = term["priceDimensions"]
        .as_object()
        .filter(|v| v.len() == 1)
        .ok_or(ProviderError)?;
    let dimension = dimensions.values().next().ok_or(ProviderError)?;
    if dimension["unit"].as_str() != Some(unit)
        || dimension["beginRange"].as_str() != Some("0")
        || dimension["endRange"].as_str() != Some("Inf")
        || !dimension["appliesTo"]
            .as_array()
            .is_some_and(|v| v.is_empty())
    {
        return Err(ProviderError);
    }
    decimal_picodollars(
        dimension["pricePerUnit"]["USD"]
            .as_str()
            .ok_or(ProviderError)?,
    )
}

fn decimal_picodollars(value: &str) -> Result<i64, ProviderError> {
    if value.len() > 64 {
        return Err(ProviderError);
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(ProviderError);
    }
    let whole = whole.parse::<i64>().map_err(|_| ProviderError)?;
    let mut fractional = 0i64;
    for index in 0..12 {
        fractional = fractional * 10
            + fraction
                .as_bytes()
                .get(index)
                .map_or(0, |b| i64::from(b - b'0'));
    }
    let round_up = fraction.as_bytes().iter().skip(12).any(|b| *b != b'0');
    whole
        .checked_mul(1_000_000_000_000)
        .and_then(|v| v.checked_add(fractional))
        .and_then(|v| v.checked_add(i64::from(round_up)))
        .filter(|v| *v > 0)
        .ok_or(ProviderError)
}

pub(super) fn fetch(
    region: &str,
    memory_mib: i32,
    timeout: Duration,
) -> Result<(ComputePrice, SystemTime), ProviderError> {
    if region.is_empty()
        || region.len() > 64
        || !region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(ProviderError);
    }
    let url = format!(
        "https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AWSLambda/current/{region}/index.json"
    );
    #[cfg(debug_assertions)]
    let url = match std::env::var("TELCHAR_TEST_MICROVM_PRICING_ENDPOINT") {
        Ok(endpoint) => {
            let parsed = url::Url::parse(&endpoint).map_err(|_| ProviderError)?;
            if parsed.scheme() != "http"
                || !parsed
                    .host_str()
                    .and_then(|host| host.parse::<std::net::Ipv4Addr>().ok())
                    .is_some_and(|ip| ip.is_loopback())
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(ProviderError);
            }
            endpoint
        }
        Err(std::env::VarError::NotPresent) => url,
        Err(_) => return Err(ProviderError),
    };
    fetch_catalog(&url, region, memory_mib, timeout)
}

fn fetch_catalog(
    url: &str,
    region: &str,
    memory_mib: i32,
    timeout: Duration,
) -> Result<(ComputePrice, SystemTime), ProviderError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ProviderError)?;
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|_| ProviderError)?;
    let mut bytes = Vec::new();
    response
        .take(MAXIMUM_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ProviderError)?;
    if bytes.len() as u64 > MAXIMUM_CATALOG_BYTES {
        return Err(ProviderError);
    }
    let catalog = serde_json::from_slice(&bytes).map_err(|_| ProviderError)?;
    let price = ComputePrice::from_catalog(&catalog, region, memory_mib)?;
    let expires = SystemTime::now()
        .checked_add(QUOTE_LIFETIME)
        .ok_or(ProviderError)?;
    Ok((price, expires))
}
