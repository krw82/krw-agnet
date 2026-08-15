//! Production deployment config parsing (doc/refetoring/05 §production config).
//!
//! The config is the single operator-authored input of the controller. It is
//! validated fail-closed: the schema version must be exactly 1, `provider` is
//! mandatory (no default and no newest-candidate selection), every path must
//! be absolute, and unknown fields are rejected so a stale config can never
//! silently select unintended behavior.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub const DEPLOY_CONFIG_SCHEMA_VERSION: u64 = 1;

/// Required local toolchain commands checked during preflight.
pub const REQUIRED_COMMANDS: [&str; 8] = ["git", "cargo", "python3", "node", "npm", "gcloud", "ssh", "docker"];

const KNOWN_TOP_LEVEL_FIELDS: [&str; 9] = [
    "schema_version",
    "provider",
    "agent_source_root",
    "frontend_source_root",
    "operator_root",
    "runtime_env",
    "target_file",
    "frontend_contract",
    "timeouts",
];

const KNOWN_TIMEOUT_FIELDS: [&str; 4] = ["ssh_ms", "mcp_ms", "daemon_ready_ms", "public_ready_ms"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployConfig {
    pub schema_version: u64,
    pub provider: String,
    pub agent_source_root: PathBuf,
    pub frontend_source_root: PathBuf,
    pub operator_root: PathBuf,
    pub runtime_env: PathBuf,
    pub target_file: PathBuf,
    pub frontend_contract: PathBuf,
    pub timeouts: DeployTimeouts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployTimeouts {
    pub ssh_ms: u64,
    pub mcp_ms: u64,
    pub daemon_ready_ms: u64,
    pub public_ready_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    InvalidJson(String),
    RootNotObject,
    UnknownField { field: String },
    MissingField { field: String },
    TypeMismatch { field: String, expected: &'static str },
    SchemaVersionUnsupported { found: u64 },
    InvalidField { field: String, reason: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(message) => write!(formatter, "config JSON invalid: {message}"),
            Self::RootNotObject => write!(formatter, "config JSON root must be an object"),
            Self::UnknownField { field } => write!(formatter, "unknown config field `{field}` rejected"),
            Self::MissingField { field } => write!(formatter, "required config field `{field}` is missing"),
            Self::TypeMismatch { field, expected } => {
                write!(formatter, "config field `{field}` must be {expected}")
            }
            Self::SchemaVersionUnsupported { found } => write!(
                formatter,
                "config schema_version {found} unsupported (expected {DEPLOY_CONFIG_SCHEMA_VERSION})"
            ),
            Self::InvalidField { field, reason } => write!(formatter, "config field `{field}` invalid: {reason}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl DeployConfig {
    /// Parse config bytes with full fail-closed validation.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ConfigError> {
        let value: Value = serde_json::from_slice(bytes).map_err(|error| ConfigError::InvalidJson(error.to_string()))?;
        Self::from_value(&value)
    }

    /// Parse config text with full fail-closed validation.
    pub fn from_json_str(text: &str) -> Result<Self, ConfigError> {
        Self::from_json_bytes(text.as_bytes())
    }

    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(path)
            .map_err(|error| ConfigError::InvalidJson(format!("cannot read config file {}: {error}", path.display())))?;
        Self::from_json_bytes(&bytes)
    }

    fn from_value(value: &Value) -> Result<Self, ConfigError> {
        let object = value.as_object().ok_or(ConfigError::RootNotObject)?;
        for field in object.keys() {
            if !KNOWN_TOP_LEVEL_FIELDS.contains(&field.as_str()) {
                return Err(ConfigError::UnknownField { field: field.clone() });
            }
        }

        let schema_version = required_u64(object, "schema_version")?;
        if schema_version != DEPLOY_CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::SchemaVersionUnsupported { found: schema_version });
        }

        let provider = required_string(object, "provider")?;
        if provider.trim().is_empty() {
            return Err(ConfigError::InvalidField {
                field: "provider".to_owned(),
                reason: "must be an explicit non-empty provider id (no default selection)".to_owned(),
            });
        }

        Ok(Self {
            schema_version,
            provider,
            agent_source_root: required_absolute_path(object, "agent_source_root")?,
            frontend_source_root: required_absolute_path(object, "frontend_source_root")?,
            operator_root: required_absolute_path(object, "operator_root")?,
            runtime_env: required_absolute_path(object, "runtime_env")?,
            target_file: required_absolute_path(object, "target_file")?,
            frontend_contract: required_absolute_path(object, "frontend_contract")?,
            timeouts: Self::parse_timeouts(object)?,
        })
    }

    fn parse_timeouts(object: &serde_json::Map<String, Value>) -> Result<DeployTimeouts, ConfigError> {
        let timeouts_value = object.get("timeouts").ok_or(ConfigError::MissingField { field: "timeouts".to_owned() })?;
        let timeouts = timeouts_value
            .as_object()
            .ok_or(ConfigError::TypeMismatch { field: "timeouts".to_owned(), expected: "an object" })?;
        for field in timeouts.keys() {
            if !KNOWN_TIMEOUT_FIELDS.contains(&field.as_str()) {
                return Err(ConfigError::UnknownField { field: format!("timeouts.{field}") });
            }
        }
        let parse = |field: &'static str| -> Result<u64, ConfigError> {
            let millis = required_u64(timeouts, field).map_err(|error| match error {
                ConfigError::MissingField { .. } => ConfigError::MissingField { field: format!("timeouts.{field}") },
                ConfigError::TypeMismatch { .. } => ConfigError::TypeMismatch {
                    field: format!("timeouts.{field}"),
                    expected: "a positive integer",
                },
                other => other,
            })?;
            if millis == 0 {
                return Err(ConfigError::InvalidField {
                    field: format!("timeouts.{field}"),
                    reason: "must be greater than zero".to_owned(),
                });
            }
            Ok(millis)
        };
        Ok(DeployTimeouts {
            ssh_ms: parse("ssh_ms")?,
            mcp_ms: parse("mcp_ms")?,
            daemon_ready_ms: parse("daemon_ready_ms")?,
            public_ready_ms: parse("public_ready_ms")?,
        })
    }

    /// Deterministic receipt directory name component for this config.
    pub fn provider_slug(&self) -> &str {
        &self.provider
    }
}

fn required_string(object: &serde_json::Map<String, Value>, field: &str) -> Result<String, ConfigError> {
    let value = object.get(field).ok_or(ConfigError::MissingField { field: field.to_owned() })?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or(ConfigError::TypeMismatch { field: field.to_owned(), expected: "a string" })
}

fn required_u64(object: &serde_json::Map<String, Value>, field: &str) -> Result<u64, ConfigError> {
    let value = object.get(field).ok_or(ConfigError::MissingField { field: field.to_owned() })?;
    value
        .as_u64()
        .ok_or(ConfigError::TypeMismatch { field: field.to_owned(), expected: "a positive integer" })
}

fn required_absolute_path(object: &serde_json::Map<String, Value>, field: &str) -> Result<PathBuf, ConfigError> {
    let text = required_string(object, field)?;
    let path = PathBuf::from(&text);
    if !path.is_absolute() {
        return Err(ConfigError::InvalidField {
            field: field.to_owned(),
            reason: format!("must be an absolute path, got relative path `{text}`"),
        });
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config_json() -> String {
        r#"{
          "schema_version": 1,
          "provider": "deepseek",
          "agent_source_root": "/tmp/krw-agent-deploy-test/agent",
          "frontend_source_root": "/tmp/krw-agent-deploy-test/front",
          "operator_root": "/tmp/krw-agent-deploy-test/operator",
          "runtime_env": "/tmp/krw-agent-deploy-test/operator/runtime/krw-agent-deploy.env",
          "target_file": "/tmp/krw-agent-deploy-test/operator/ops/production-target.json",
          "frontend_contract": "/tmp/krw-agent-deploy-test/operator/ops/agent-v1-deployment-contract.json",
          "timeouts": {
            "ssh_ms": 5000,
            "mcp_ms": 5000,
            "daemon_ready_ms": 90000,
            "public_ready_ms": 90000
          }
        }"#
        .to_owned()
    }

    #[test]
    fn valid_minimal_config_parses() {
        let config = DeployConfig::from_json_str(&minimal_config_json()).unwrap();
        assert_eq!(config.schema_version, 1);
        assert_eq!(config.provider, "deepseek");
        assert_eq!(config.agent_source_root, PathBuf::from("/tmp/krw-agent-deploy-test/agent"));
        assert_eq!(config.timeouts.ssh_ms, 5000);
        assert_eq!(config.timeouts.daemon_ready_ms, 90000);
    }

    #[test]
    fn relative_path_is_rejected() {
        let text = minimal_config_json().replace(
            "\"/tmp/krw-agent-deploy-test/agent\"",
            "\"krw-agent-relative/agent\"",
        );
        let error = DeployConfig::from_json_str(&text).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidField { ref field, .. } if field == "agent_source_root"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn missing_provider_is_rejected() {
        let text = minimal_config_json().replace("\"provider\": \"deepseek\",", "");
        let error = DeployConfig::from_json_str(&text).unwrap_err();
        assert!(
            matches!(error, ConfigError::MissingField { ref field } if field == "provider"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn empty_provider_is_rejected() {
        let text = minimal_config_json().replace("\"provider\": \"deepseek\"", "\"provider\": \"\"");
        let error = DeployConfig::from_json_str(&text).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidField { ref field, .. } if field == "provider"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn schema_version_two_is_rejected() {
        let text = minimal_config_json().replace("\"schema_version\": 1", "\"schema_version\": 2");
        let error = DeployConfig::from_json_str(&text).unwrap_err();
        assert!(
            matches!(error, ConfigError::SchemaVersionUnsupported { found: 2 }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        let text = minimal_config_json().replace(
            "\"timeouts\": {",
            "\"rollback_branch\": \"main\",\n          \"timeouts\": {",
        );
        let error = DeployConfig::from_json_str(&text).unwrap_err();
        assert!(
            matches!(error, ConfigError::UnknownField { ref field } if field == "rollback_branch"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unknown_timeout_field_is_rejected() {
        let text = minimal_config_json().replace("\"ssh_ms\": 5000", "\"ssh_ms\": 5000, \"http_ms\": 1000");
        let error = DeployConfig::from_json_str(&text).unwrap_err();
        assert!(
            matches!(error, ConfigError::UnknownField { ref field } if field == "timeouts.http_ms"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zero_timeout_is_rejected() {
        let text = minimal_config_json().replace("\"mcp_ms\": 5000", "\"mcp_ms\": 0");
        let error = DeployConfig::from_json_str(&text).unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidField { ref field, .. } if field == "timeouts.mcp_ms"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn missing_timeouts_object_is_rejected() {
        let text = minimal_config_json();
        let start = text.find("\"timeouts\"").unwrap();
        let text = format!("{}}}", text[..start].trim_end().trim_end_matches(','));
        let error = DeployConfig::from_json_str(&text).unwrap_err();
        assert!(
            matches!(error, ConfigError::MissingField { ref field } if field == "timeouts"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn non_object_root_is_rejected() {
        let error = DeployConfig::from_json_str("[]").unwrap_err();
        assert!(matches!(error, ConfigError::RootNotObject), "unexpected error: {error}");
    }

    #[test]
    fn invalid_json_is_rejected() {
        let error = DeployConfig::from_json_str("{ not json").unwrap_err();
        assert!(matches!(error, ConfigError::InvalidJson(_)), "unexpected error: {error}");
    }

    #[test]
    fn required_commands_match_the_05_doc_toolchain_list() {
        assert_eq!(
            REQUIRED_COMMANDS,
            ["git", "cargo", "python3", "node", "npm", "gcloud", "ssh", "docker"]
        );
    }
}
