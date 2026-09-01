//! Operator debugging tool: decrypt one run's provider episodes from the
//! local artifact store and write each plaintext to an output directory.
//!
//! Usage:
//!   dump_episode <store_root> <tenant_id> <principal_id> <run_id> <refs_jsonl> <out_dir>
//!
//! `refs_jsonl` holds one provider-episode `episode_artifact_ref` JSON object
//! per line (as stored in `agent_store.provider_episodes`). The master key is
//! read from `KRW_AGENT_ARTIFACT_KEY_V1` (64 hex chars) and never printed.

use krw_agent_artifact_store::{
    ArtifactScope, ArtifactStore, ArtifactStoreConfig, LocalArtifactStore, MasterKeyring,
    VersionedMasterKey,
};
use std::io::BufRead;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 7 {
        eprintln!(
            "usage: dump_episode <store_root> <tenant_id> <principal_id> <run_id> <refs_jsonl> <out_dir>"
        );
        std::process::exit(2);
    }
    let store_root = std::path::PathBuf::from(&args[1]);
    let tenant = args[2].clone();
    let principal = args[3].clone();
    let run_id = args[4].clone();
    let refs_path = std::path::PathBuf::from(&args[5]);
    let out_dir = std::path::PathBuf::from(&args[6]);

    let key_hex = std::env::var("KRW_AGENT_ARTIFACT_KEY_V1").expect("KRW_AGENT_ARTIFACT_KEY_V1");
    let key_bytes = hex::decode(key_hex.trim()).expect("key must be hex");
    let mut material = [0u8; 32];
    material.copy_from_slice(&key_bytes);

    let keyring = MasterKeyring::new(
        1,
        [VersionedMasterKey::new(1, material).expect("valid key")],
    )
    .expect("valid keyring");

    let store = LocalArtifactStore::open(&store_root, keyring, ArtifactStoreConfig::default())
        .expect("open artifact store");
    let scope = ArtifactScope::new(tenant, principal, run_id).expect("valid scope");
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let file = std::fs::File::open(&refs_path).expect("open refs jsonl");
    let mut index = 0u32;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.expect("read line");
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        index += 1;
        let reference: krw_agent_artifact_store::ArtifactRef =
            serde_json::from_str(line).expect("parse artifact ref");
        let plaintext = runtime
            .block_on(store.get(&scope, &reference))
            .unwrap_or_else(|error| panic!("episode {index}: {error}"));
        let out_path = out_dir.join(format!("ep{index:02}.json"));
        std::fs::write(&out_path, plaintext.expose()).expect("write episode");
        println!("episode {index}: {} bytes -> {}", plaintext.expose().len(), out_path.display());
    }
}
