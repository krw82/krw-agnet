//! Frontend deployment contract validation and canonical hashing.
//!
//! Port of `scripts/verify_frontend_deployment_contract.py`: the frontend
//! owns exactly one canonical contract artifact and the agent side only
//! verifies that file. The hash is the SHA-256 of the canonical JSON
//! (sorted keys, compact separators, UTF-8) of the parsed value — not of the
//! raw file bytes — so formatting churn on the frontend side cannot change
//! the pinned identity.

use std::fmt;
use std::path::Path;

use krw_agent_protocol::ContentHash;
use serde_json::Value;

pub const FRONTEND_CONTRACT_ID: &str = "krw.agent/frontend-deployment-contract/v1";
pub const FRONTEND_CONTRACT_AGENT_ABI: &str = "agent_v1_v7";
pub const FRONTEND_CONTRACT_SCHEMA_VERSION: i64 = 1;

const CORE_PROJECTION_FIELDS: [&str; 4] = ["run_id", "answer_bundle_hash", "final_output_hash", "markdown"];
const OPTIONAL_PROJECTION_FIELDS: [&str; 5] = [
    "visualizations",
    "usage",
    "evidence_ledger_hash",
    "memory_revision",
    "memory_frontier_hash",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontendContract {
    /// `sha256:<hex>` over the canonical JSON bytes.
    pub canonical_sha256: String,
    pub contract_id: String,
    pub agent_abi: String,
    pub final_projection_contract: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    MissingOrUnsafe(String),
    JsonInvalid(String),
    RootInvalid,
    FieldsMismatch(&'static str),
    ValueInvalid { field: &'static str, expected: String },
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOrUnsafe(path) => {
                write!(formatter, "frontend contract missing or unsafe: {path}")
            }
            Self::JsonInvalid(message) => write!(formatter, "frontend contract JSON invalid: {message}"),
            Self::RootInvalid => write!(formatter, "frontend contract root must be an object"),
            Self::FieldsMismatch(field) => write!(formatter, "frontend contract field missing: {field}"),
            Self::ValueInvalid { field, expected } => {
                write!(formatter, "frontend contract field `{field}` must be {expected}")
            }
        }
    }
}

impl std::error::Error for ContractError {}

/// Load, validate, and canonically hash the frontend deployment contract.
pub fn load_contract(path: &Path) -> Result<FrontendContract, ContractError> {
    if path.is_symlink() || !path.is_file() {
        return Err(ContractError::MissingOrUnsafe(path.display().to_string()));
    }
    let bytes = std::fs::read(path).map_err(|error| ContractError::MissingOrUnsafe(format!("{}: {error}", path.display())))?;
    parse_contract(&bytes)
}

pub fn parse_contract(bytes: &[u8]) -> Result<FrontendContract, ContractError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| ContractError::JsonInvalid(error.to_string()))?;
    let object = value.as_object().ok_or(ContractError::RootInvalid)?;

    for field in REQUIRED_FIELDS {
        if !object.contains_key(field) {
            return Err(ContractError::FieldsMismatch(field));
        }
    }
    if object.get("schema_version").and_then(Value::as_i64) != Some(FRONTEND_CONTRACT_SCHEMA_VERSION) {
        return Err(ContractError::ValueInvalid { field: "schema_version", expected: "1".to_owned() });
    }
    expect_str(object, "contract_id", FRONTEND_CONTRACT_ID)?;
    expect_str(object, "agent_abi", FRONTEND_CONTRACT_AGENT_ABI)?;
    expect_str(object, "executor_type", "rust_agent")?;
    expect_str(object, "session_backend", "rust_agent")?;
    expect_str(object, "run_submission_contract", "agent-v1-run-request/v1")?;
    expect_str(object, "final_projection_contract", "agent-final-projection/v1")?;
    expect_str(object, "visualization_failure_policy", "omit_artifact_keep_answer")?;
    expect_str(object, "admission_policy", "open_after_exact_release_heartbeat")?;

    let required = string_list(object.get("required_projection_fields"));
    if required != CORE_PROJECTION_FIELDS {
        return Err(ContractError::ValueInvalid {
            field: "required_projection_fields",
            expected: CORE_PROJECTION_FIELDS.join(","),
        });
    }
    let optional = string_list(object.get("optional_projection_fields"));
    if optional.iter().any(String::is_empty)
        || optional.len() != deduped_count(&optional)
        || !OPTIONAL_PROJECTION_FIELDS.iter().all(|field| optional.contains(&(*field).to_owned()))
        || CORE_PROJECTION_FIELDS.iter().any(|field| optional.contains(&(*field).to_owned()))
    {
        return Err(ContractError::ValueInvalid {
            field: "optional_projection_fields",
            expected: OPTIONAL_PROJECTION_FIELDS.join(","),
        });
    }

    // Canonical bytes: JCS (sorted keys, compact separators) over the parsed
    // value, matching the Python verifier's json.dumps(sort_keys=True,
    // separators=(",", ":")) canonical form.
    let canonical = serde_jcs::to_vec(&value).map_err(|error| ContractError::JsonInvalid(error.to_string()))?;
    let hash = ContentHash::sha256(canonical).to_string();
    Ok(FrontendContract {
        canonical_sha256: hash,
        contract_id: FRONTEND_CONTRACT_ID.to_owned(),
        agent_abi: FRONTEND_CONTRACT_AGENT_ABI.to_owned(),
        final_projection_contract: "agent-final-projection/v1".to_owned(),
    })
}

const REQUIRED_FIELDS: [&str; 11] = [
    "schema_version",
    "contract_id",
    "agent_abi",
    "executor_type",
    "session_backend",
    "run_submission_contract",
    "final_projection_contract",
    "required_projection_fields",
    "optional_projection_fields",
    "visualization_failure_policy",
    "admission_policy",
];

fn expect_str(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    expected: &'static str,
) -> Result<(), ContractError> {
    if object.get(field).and_then(Value::as_str) != Some(expected) {
        return Err(ContractError::ValueInvalid { field, expected: expected.to_owned() });
    }
    Ok(())
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn deduped_count(values: &[String]) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    values.iter().filter(|value| seen.insert((*value).clone())).count()
}

/// Canonical fixture contract bytes used by tests and the fixtures directory.
pub fn canonical_fixture_contract_json() -> String {
    r#"{
  "schema_version": 1,
  "contract_id": "krw.agent/frontend-deployment-contract/v1",
  "agent_abi": "agent_v1_v7",
  "executor_type": "rust_agent",
  "session_backend": "rust_agent",
  "run_submission_contract": "agent-v1-run-request/v1",
  "final_projection_contract": "agent-final-projection/v1",
  "required_projection_fields": ["run_id", "answer_bundle_hash", "final_output_hash", "markdown"],
  "optional_projection_fields": [
    "visualizations",
    "usage",
    "evidence_ledger_hash",
    "memory_revision",
    "memory_frontier_hash"
  ],
  "visualization_failure_policy": "omit_artifact_keep_answer",
  "admission_policy": "open_after_exact_release_heartbeat"
}
"#
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cross-checked against the Python verifier's canonical form:
    // json.dumps(sort_keys=True, separators=(",", ":")).encode("utf-8").
    const FIXTURE_CANONICAL_SHA256: &str = "sha256:164482f3fe99eb82977bcd77c07792d0472cae014f8218a0c492f42c1a29055e";

    #[test]
    fn canonical_fixture_contract_validates_and_hashes_stably() {
        let contract = parse_contract(canonical_fixture_contract_json().as_bytes()).unwrap();
        assert_eq!(contract.agent_abi, "agent_v1_v7");
        assert_eq!(contract.canonical_sha256, FIXTURE_CANONICAL_SHA256);
    }

    #[test]
    fn hash_is_format_insensitive() {
        // Same value, different whitespace: identical canonical hash.
        let pretty = serde_json::to_string_pretty(
            &serde_json::from_str::<Value>(&canonical_fixture_contract_json()).unwrap(),
        )
        .unwrap();
        let contract = parse_contract(pretty.as_bytes()).unwrap();
        assert_eq!(contract.canonical_sha256, FIXTURE_CANONICAL_SHA256);
    }

    #[test]
    fn wrong_abi_is_rejected() {
        let text = canonical_fixture_contract_json().replace("agent_v1_v7", "agent_v2");
        let error = parse_contract(text.as_bytes()).unwrap_err();
        assert!(
            matches!(error, ContractError::ValueInvalid { field: "agent_abi", .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn missing_field_is_rejected() {
        let text = canonical_fixture_contract_json().replace("\"admission_policy\": \"open_after_exact_release_heartbeat\"", "\"admission_policy_x\": \"open_after_exact_release_heartbeat\"");
        let error = parse_contract(text.as_bytes()).unwrap_err();
        assert!(
            matches!(error, ContractError::FieldsMismatch("admission_policy")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn wrong_required_projection_fields_are_rejected() {
        let text = canonical_fixture_contract_json().replace("\"markdown\"", "\"markdown_v2\"");
        let error = parse_contract(text.as_bytes()).unwrap_err();
        assert!(
            matches!(error, ContractError::ValueInvalid { field: "required_projection_fields", .. }),
            "unexpected error: {error}"
        );
    }
}
