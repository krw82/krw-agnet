//! Operator production-target file model (doc/refetoring/05 preflight input).
//!
//! The target file is operator-owned deployment topology data. Unlike the
//! strict deployment config, unknown fields are tolerated (the file is
//! expected to grow), but provider selection is validated fail-closed:
//! exactly one explicit provider, no arrays of candidates, no legacy
//! dual/newest selection keys.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetFile {
    /// Single explicit provider id (e.g. `deepseek`). Required by preflight.
    pub provider: Option<String>,
    pub gcp: Option<GcpTarget>,
    pub ssh_host: Option<String>,
    /// Local ports the activated release must own exclusively.
    pub local_ports: Vec<u16>,
    /// Env key NAMES (never values) the remote runtime env must provide.
    pub required_env_keys: Vec<String>,
    /// Process-env variable name carrying the Supabase/Postgres URL handle.
    pub db_url_env: Option<String>,
    /// Declared DB agent ABI (e.g. `agent_v1_v7`).
    pub db_abi: Option<String>,
    /// Remote MCP endpoints as `host:port` strings (connectivity only, no LLM calls).
    pub mcp_endpoints: Vec<String>,
    /// Remote frontend directory on the deploy target (used by ssh-driven
    /// admission/activation stages). Absent together with `ssh_host` marks a
    /// local-only deployment.
    pub remote_front_dir: Option<String>,
    /// Remote docker compose project name (defaults to the basename of
    /// `remote_front_dir`).
    pub compose_project: Option<String>,
    /// Supabase forward migration plan entries (read-only ordering proof).
    pub supabase_migration_plan: Vec<String>,
    /// Operator-pinned `sha256:<hex>` of the canonical frontend contract JSON.
    pub frontend_contract_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcpTarget {
    pub project: String,
    pub zone: String,
    pub instance: String,
    pub instance_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    Read(String),
    InvalidJson(String),
    RootNotObject,
    ProviderInvalid(String),
}

impl fmt::Display for TargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(message) => write!(formatter, "target file unreadable: {message}"),
            Self::InvalidJson(message) => write!(formatter, "target file JSON invalid: {message}"),
            Self::RootNotObject => write!(formatter, "target file root must be an object"),
            Self::ProviderInvalid(message) => write!(formatter, "target provider invalid: {message}"),
        }
    }
}

impl std::error::Error for TargetError {}

/// Keys whose presence means the operator (or a legacy script) left an
/// ambiguous multi-candidate provider selection in the target file.
const AMBIGUOUS_PROVIDER_KEYS: [&str; 3] = ["providers", "provider_candidates", "provider_alias"];

impl TargetFile {
    pub fn from_path(path: &Path) -> Result<Self, TargetError> {
        let bytes = std::fs::read(path).map_err(|error| TargetError::Read(format!("{}: {error}", path.display())))?;
        Self::from_json_bytes(&bytes)
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, TargetError> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|error| TargetError::InvalidJson(error.to_string()))?;
        let object = value.as_object().ok_or(TargetError::RootNotObject)?;
        Self::from_object(object)
    }

    fn from_object(object: &serde_json::Map<String, Value>) -> Result<Self, TargetError> {
        for key in AMBIGUOUS_PROVIDER_KEYS {
            if object.contains_key(key) {
                return Err(TargetError::ProviderInvalid(format!(
                    "ambiguous selection key `{key}` present; exactly one explicit provider is required"
                )));
            }
        }
        let provider = match object.get("provider") {
            None => None,
            Some(Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
            Some(other) => {
                return Err(TargetError::ProviderInvalid(format!(
                    "`provider` must be a single non-empty string, got {other}"
                )));
            }
        };
        let string_field = |key: &str| -> Option<String> {
            object.get(key).and_then(Value::as_str).map(str::to_owned)
        };
        let string_list = |key: &str| -> Vec<String> {
            object
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let gcp = object.get("gcp").and_then(Value::as_object).map(|gcp| GcpTarget {
            project: gcp.get("project").and_then(Value::as_str).unwrap_or_default().to_owned(),
            zone: gcp.get("zone").and_then(Value::as_str).unwrap_or_default().to_owned(),
            instance: gcp.get("instance").and_then(Value::as_str).unwrap_or_default().to_owned(),
            instance_id: gcp.get("instance_id").and_then(Value::as_str).map(str::to_owned),
        });
        Ok(Self {
            provider,
            gcp,
            ssh_host: string_field("ssh_host"),
            remote_front_dir: string_field("remote_front_dir"),
            compose_project: string_field("compose_project"),
            local_ports: object
                .get("local_ports")
                .and_then(Value::as_array)
                .map(|ports| {
                    ports
                        .iter()
                        .filter_map(Value::as_u64)
                        .filter_map(|port| u16::try_from(port).ok())
                        .filter(|port| *port != 0)
                        .collect::<Vec<u16>>()
                })
                .unwrap_or_default(),
            required_env_keys: string_list("required_env_keys"),
            db_url_env: string_field("db_url_env"),
            db_abi: string_field("db_abi"),
            mcp_endpoints: string_list("mcp_endpoints"),
            supabase_migration_plan: string_list("supabase_migration_plan"),
            frontend_contract_sha256: string_field("frontend_contract_sha256").as_deref().map(normalize_hash_pin),
        })
    }

    /// Receipt identity fields: provider plus any recorded GCP target identity.
    pub fn identity_map(&self) -> BTreeMap<String, String> {
        let mut identity = BTreeMap::new();
        if let Some(provider) = &self.provider {
            identity.insert("provider".to_owned(), provider.clone());
        }
        if let Some(gcp) = &self.gcp {
            if !gcp.project.is_empty() {
                identity.insert("gcp_project".to_owned(), gcp.project.clone());
            }
            if !gcp.zone.is_empty() {
                identity.insert("gcp_zone".to_owned(), gcp.zone.clone());
            }
            if !gcp.instance.is_empty() {
                identity.insert("gcp_instance".to_owned(), gcp.instance.clone());
            }
            if let Some(instance_id) = &gcp.instance_id {
                identity.insert("gcp_instance_id".to_owned(), instance_id.clone());
            }
        }
        identity
    }
}

/// Accept `sha256:<hex>` or a bare 64-char hex pin; always normalize to the
/// `sha256:<lowercase-hex>` form used by receipts and contract hashing.
fn normalize_hash_pin(pin: &str) -> String {
    let hex_part = pin.strip_prefix("sha256:").unwrap_or(pin).to_ascii_lowercase();
    format!("sha256:{hex_part}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_target_json() -> String {
        r#"{
          "provider": "deepseek",
          "gcp": {
            "project": "krw-prod-dummy",
            "zone": "asia-northeast3-a",
            "instance": "krw-agent-prod-dummy",
            "instance_id": "1234567890123456789"
          },
          "ssh_host": "deploy@127.0.0.1",
          "local_ports": [18080],
          "required_env_keys": ["KRW_AGENT_DB_URL", "KRW_AGENT_PROVIDER_KEY_HANDLE"],
          "db_url_env": "KRW_AGENT_DB_URL",
          "db_abi": "agent_v1_v7",
          "mcp_endpoints": ["127.0.0.1:18081"],
          "supabase_migration_plan": ["0001_agent_v1.sql", "0002_heartbeats.sql"],
          "frontend_contract_sha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }"#
        .to_owned()
    }

    #[test]
    fn minimal_target_parses() {
        let target = TargetFile::from_json_bytes(minimal_target_json().as_bytes()).unwrap();
        assert_eq!(target.provider.as_deref(), Some("deepseek"));
        let gcp = target.gcp.unwrap();
        assert_eq!(gcp.project, "krw-prod-dummy");
        assert_eq!(gcp.instance_id.as_deref(), Some("1234567890123456789"));
        assert_eq!(target.local_ports, [18080]);
        assert_eq!(target.required_env_keys.len(), 2);
        assert_eq!(target.mcp_endpoints, ["127.0.0.1:18081"]);
        assert_eq!(target.supabase_migration_plan.len(), 2);
        assert_eq!(
            target.frontend_contract_sha256.as_deref(),
            Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn missing_provider_parses_but_reports_none() {
        let text = minimal_target_json().replace("\"provider\": \"deepseek\",", "");
        let target = TargetFile::from_json_bytes(text.as_bytes()).unwrap();
        assert!(target.provider.is_none());
    }

    #[test]
    fn provider_array_is_rejected_as_ambiguous() {
        let text = minimal_target_json().replace("\"provider\": \"deepseek\"", "\"provider\": [\"glm\", \"deepseek\"]");
        let error = TargetFile::from_json_bytes(text.as_bytes()).unwrap_err();
        assert!(matches!(error, TargetError::ProviderInvalid(_)), "unexpected error: {error}");
    }

    #[test]
    fn legacy_candidate_key_is_rejected() {
        let text = minimal_target_json().replace(
            "\"provider\": \"deepseek\",",
            "\"provider\": \"deepseek\",\n  \"provider_candidates\": [\"glm\", \"deepseek\"],",
        );
        let error = TargetFile::from_json_bytes(text.as_bytes()).unwrap_err();
        assert!(
            matches!(error, TargetError::ProviderInvalid(ref message) if message.contains("provider_candidates")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn hash_pin_without_prefix_is_normalized() {
        let text = minimal_target_json().replace(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        );
        let target = TargetFile::from_json_bytes(text.as_bytes()).unwrap();
        assert_eq!(
            target.frontend_contract_sha256.as_deref(),
            Some("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[test]
    fn identity_map_records_provider_and_gcp() {
        let target = TargetFile::from_json_bytes(minimal_target_json().as_bytes()).unwrap();
        let identity = target.identity_map();
        assert_eq!(identity.get("provider").map(String::as_str), Some("deepseek"));
        assert_eq!(identity.get("gcp_zone").map(String::as_str), Some("asia-northeast3-a"));
        assert_eq!(identity.get("gcp_instance_id").map(String::as_str), Some("1234567890123456789"));
    }

    #[test]
    fn remote_topology_fields_parse_and_default_to_none() {
        let target = TargetFile::from_json_bytes(minimal_target_json().as_bytes()).unwrap();
        assert!(target.remote_front_dir.is_none());
        assert!(target.compose_project.is_none());

        let text = minimal_target_json().replace(
            "\"ssh_host\": \"deploy@127.0.0.1\",",
            "\"ssh_host\": \"deploy@127.0.0.1\",\n  \"remote_front_dir\": \"/home/deploy/krw-ontology-front\",\n  \"compose_project\": \"krw-ontology-front\",",
        );
        let target = TargetFile::from_json_bytes(text.as_bytes()).unwrap();
        assert_eq!(target.remote_front_dir.as_deref(), Some("/home/deploy/krw-ontology-front"));
        assert_eq!(target.compose_project.as_deref(), Some("krw-ontology-front"));
    }
}
