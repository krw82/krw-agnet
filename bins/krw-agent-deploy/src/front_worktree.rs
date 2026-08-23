//! Automatic clean-worktree materialization for the frontend source.
//!
//! The `frontend-source-clean-commit` preflight check was the single
//! largest deploy-failure cause in the 2026-08 receipts (11 of 45).
//! This module automates the operator's manual workaround: when the
//! configured frontend checkout is dirty, materialize a detached git
//! worktree at its HEAD and build from that instead.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve a clean frontend source for the sealed build.
///
/// The preflight's `frontend-source-clean-commit` check failed 11 times
/// in the 2026-08 receipts — every one blocking recovery of an
/// already-closed admission. When the configured source is dirty we now
/// materialize a detached worktree at its HEAD (the same
/// `/tmp/krw-front-sealed` pattern the operator ran manually) instead of
/// failing. Sealed builds always compile a clean committed tree; the
/// operator's working checkout is never touched.
///
/// A clean source is returned unchanged. An already-materialized
/// `front-<head>` directory under `dest_root` is reused, so repeated
/// runs against the same HEAD do not accumulate new worktrees (note that
/// stale registrations in the *source* repo linger until
/// `git worktree prune` is run there; this function never prunes).
///
/// A fresh worktree has none of the untracked build artifacts the local
/// verification scripts need: `node_modules` (stage-6 RPC contract
/// verify resolved `typescript` from it — production run
/// 20260823T095142Z), `supabase/.temp` (schema-smoke reads
/// `.temp/project-ref` — run 20260823T095730Z), and the local env
/// files. The same set the operator's manual sealed-worktree recipe
/// symlinked. Each is symlinked from the source checkout for new AND
/// reused worktrees, so a run that died mid-way still gets backfilled.
pub fn materialize_clean_front_worktree(
    front_root: &Path,
    dest_root: &Path,
) -> Result<PathBuf, String> {
    let porcelain = Command::new("git")
        .args([
            "-C",
            front_root.to_string_lossy().as_ref(),
            "status",
            "--porcelain",
        ])
        .output()
        .map_err(|e| format!("cannot spawn git: {e}"))?;
    if !porcelain.status.success() {
        return Err("git status failed on the frontend source".to_owned());
    }
    if porcelain.stdout.is_empty() {
        return Ok(front_root.to_path_buf());
    }
    let head = Command::new("git")
        .args([
            "-C",
            front_root.to_string_lossy().as_ref(),
            "rev-parse",
            "HEAD",
        ])
        .output()
        .map_err(|e| format!("cannot spawn git: {e}"))?;
    if !head.status.success() {
        return Err("git rev-parse HEAD failed".to_owned());
    }
    let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    let worktree = dest_root.join(format!("front-{head}"));
    if worktree.exists() {
        ensure_artifact_links(front_root, &worktree)?;
        return Ok(worktree);
    }
    std::fs::create_dir_all(dest_root)
        .map_err(|e| format!("cannot create {}: {e}", dest_root.display()))?;
    let status = Command::new("git")
        .args([
            "-C",
            front_root.to_string_lossy().as_ref(),
            "worktree",
            "add",
            "--detach",
            worktree.to_string_lossy().as_ref(),
            &head,
        ])
        .status()
        .map_err(|e| format!("cannot spawn git: {e}"))?;
    if !status.success() {
        return Err(format!("git worktree add failed for HEAD {head}"));
    }
    ensure_artifact_links(front_root, &worktree)?;
    Ok(worktree)
}

/// Untracked artifacts the local deploy scripts resolve from the front
/// checkout. Linked one-by-one, only when present in the source and
/// absent in the worktree.
const LINKED_ARTIFACTS: &[&str] = &[
    "node_modules",
    "supabase/.temp",
    ".env",
    ".env.production",
];

/// Symlink each untracked artifact from the source checkout into a
/// materialized worktree. No-op per artifact when the worktree already
/// resolves one or the source has none.
fn ensure_artifact_links(front_root: &Path, worktree: &Path) -> Result<(), String> {
    for relative in LINKED_ARTIFACTS {
        let source_artifact = front_root.join(relative);
        let worktree_artifact = worktree.join(relative);
        if !source_artifact.exists() || worktree_artifact.exists() {
            continue;
        }
        if let Some(parent) = worktree_artifact.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!("cannot create {}: {e}", parent.display())
            })?;
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&source_artifact, &worktree_artifact).map_err(|e| {
                format!(
                    "cannot link {relative} into {}: {e}",
                    worktree.display()
                )
            })?;
        }
        #[cfg(not(unix))]
        {
            let _ = source_artifact;
            return Err("artifact linking is only supported on unix".to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod worktree_tests {
    use super::materialize_clean_front_worktree;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn make_repo(dir: &Path, dirty: bool) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join(".gitignore"), "node_modules\n").unwrap();
        std::fs::write(dir.join("f.txt"), "v1").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "1"]);
        if dirty {
            std::fs::write(dir.join("f.txt"), "v2 uncommitted").unwrap();
        }
    }

    #[test]
    fn clean_source_is_returned_untouched() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        make_repo(src.path(), false);
        let resolved = materialize_clean_front_worktree(src.path(), dest.path()).unwrap();
        assert_eq!(resolved, src.path());
    }

    #[test]
    fn dirty_source_yields_a_clean_detached_worktree() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        make_repo(src.path(), true);
        std::fs::create_dir_all(src.path().join("node_modules/typescript")).unwrap();
        let resolved = materialize_clean_front_worktree(src.path(), dest.path()).unwrap();
        assert_ne!(resolved, src.path(), "must not build from the dirty tree");
        assert!(resolved.starts_with(dest.path()));
        let porcelain = Command::new("git")
            .args(["-C", resolved.to_str().unwrap(), "status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            porcelain.stdout.is_empty(),
            "worktree must be clean at HEAD"
        );
    }

    #[test]
    fn worktree_reuses_source_artifacts_and_backfills_on_reuse() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        make_repo(src.path(), true);
        std::fs::create_dir_all(src.path().join("node_modules/typescript")).unwrap();
        std::fs::create_dir_all(src.path().join("supabase/.temp")).unwrap();
        std::fs::write(src.path().join("supabase/.temp/project-ref"), "ref-1").unwrap();
        std::fs::write(src.path().join(".env.production"), "NEXT_PUBLIC_APP_URL=x").unwrap();
        let resolved = materialize_clean_front_worktree(src.path(), dest.path()).unwrap();
        let link = resolved.join("node_modules");
        assert!(
            link.symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false),
            "node_modules must be a symlink to the source checkout's install"
        );
        assert!(link.join("typescript").is_dir());
        assert!(resolved.join("supabase/.temp/project-ref").is_file());
        assert!(resolved.join(".env.production").is_file());

        // A run that died between worktree add and the links gets backfilled
        // on reuse (production runs 20260823T095142Z / 095730Z).
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_file(resolved.join("supabase/.temp")).unwrap();
        let reused = materialize_clean_front_worktree(src.path(), dest.path()).unwrap();
        assert_eq!(reused, resolved);
        assert!(reused.join("node_modules/typescript").is_dir());
        assert!(reused.join("supabase/.temp/project-ref").is_file());
    }
}
