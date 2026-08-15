//! Content hashing helpers shared by preflight checks and receipts.

use std::path::Path;

use krw_agent_protocol::ContentHash;

/// `sha256:<hex>` of file bytes, or a clear error for missing/unreadable
/// files. Receipts record `<unavailable>` placeholders instead of failing to
/// serialize when an input could not be hashed.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(ContentHash::sha256(bytes).to_string())
}

pub const UNAVAILABLE_HASH: &str = "<unavailable>";

/// Hash or record the unavailable marker (receipts must stay serializable).
pub fn sha256_file_or_unavailable(path: &Path) -> String {
    sha256_file(path).unwrap_or_else(|_| UNAVAILABLE_HASH.to_owned())
}

/// `sha256:<hex>` of arbitrary bytes.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    ContentHash::sha256(bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_file_matches_known_digest() {
        let path = std::env::temp_dir().join(format!("krw-agent-deploy-hash-test-{}", std::process::id()));
        std::fs::write(&path, b"krw-agent-deploy").unwrap();
        let digest = sha256_file(&path).unwrap();
        // Cross-checked: python3 -c 'import hashlib; print(hashlib.sha256(b"krw-agent-deploy").hexdigest())'
        assert_eq!(
            digest,
            "sha256:5e546387c15afd2bf76225c02f6aeb365c8c013312fb03fbc1e62d53ec1c90c9"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_unavailable() {
        assert_eq!(sha256_file_or_unavailable(Path::new("/nonexistent/krw-agent-deploy/nope")), UNAVAILABLE_HASH);
    }
}
