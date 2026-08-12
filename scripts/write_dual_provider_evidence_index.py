#!/usr/bin/env python3
"""Assemble a secret-free go/no-go evidence index for one dual release.

The command only consumes already-produced, bounded JSON receipts and sealed
bundles. It never calls a provider or database and it never infers a pass from
missing evidence. The resulting index binds the agent release to the front
commit, host package provenance, DB compatibility gate, both provider
acceptance reports, and the DeepSeek product canary.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import tempfile
from typing import Any

from release_provider import PROVIDER_MODELS, model_for_provider, validate_public_descriptor
from verify_standalone_release import canonical_bytes, content_hash, verify_bundle


HASH_RE = "sha256:"
MAX_INPUT_BYTES = 8 * 1024 * 1024
PROVIDERS = ("glm", "deepseek")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-root", required=True, type=pathlib.Path)
    parser.add_argument("--front-commit", required=True)
    parser.add_argument("--host-provenance", required=True, type=pathlib.Path)
    parser.add_argument("--db-compatibility", required=True, type=pathlib.Path)
    parser.add_argument("--glm-evidence", required=True, type=pathlib.Path)
    parser.add_argument("--deepseek-evidence", required=True, type=pathlib.Path)
    parser.add_argument("--deepseek-canary", required=True, type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    return parser.parse_args()


def valid_hash(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 71
        and value.startswith(HASH_RE)
        and all(character in "0123456789abcdef" for character in value[7:])
    )


def bounded_json(path: pathlib.Path, label: str) -> tuple[dict[str, Any], bytes]:
    if not path.is_absolute() or path.is_symlink() or not path.is_file():
        raise ValueError(f"{label} must be an absolute regular file")
    size = path.stat().st_size
    if size <= 0 or size > MAX_INPUT_BYTES:
        raise ValueError(f"{label} exceeds the bounded input size")
    raw = path.read_bytes()
    value = json.loads(raw)
    if not isinstance(value, dict):
        raise ValueError(f"{label} must contain an object")
    return value, raw


def validate_commit(value: Any, label: str) -> str:
    if not isinstance(value, str) or not (len(value) in (40, 64)):
        raise ValueError(f"{label} is not a Git object id")
    if any(character not in "0123456789abcdef" for character in value):
        raise ValueError(f"{label} is not a lowercase Git object id")
    return value


def validate_sealed_bundle(root: pathlib.Path, provider: str) -> dict[str, Any]:
    if not root.is_absolute() or root.is_symlink() or not root.is_dir():
        raise ValueError(f"{provider} bundle is not an absolute real directory")
    report = verify_bundle(root)
    if report["provider_id"] != provider or report["physical_models"] != [model_for_provider(provider)]:
        raise ValueError(f"{provider} bundle provider/model mismatch")
    required = (
        "public-release.json",
        "release-authorization.json",
        "release-trust-registry.json",
        "frontend-runtime.env",
    )
    for name in required:
        path = root / name
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"{provider} bundle is missing sealed artifact: {name}")
    descriptor, descriptor_raw = bounded_json(root / "public-release.json", f"{provider} descriptor")
    if descriptor.get("schema_version") != 3 or not valid_hash(descriptor.get("release_set_hash")):
        raise ValueError(f"{provider} descriptor is not release schema v3")
    if canonical_bytes(descriptor) != descriptor_raw:
        raise ValueError(f"{provider} descriptor is not canonical JSON")
    descriptor_hash = content_hash(descriptor_raw)
    validate_public_descriptor(
        (root / "public-release.json").resolve(),
        provider,
        expected_artifact_hash=descriptor_hash,
        expected_release_set_hash=descriptor["release_set_hash"],
    )
    runtime = (root / "frontend-runtime.env").read_text(encoding="utf-8")
    if f"KRW_AGENT_PROVIDER={provider}\n" not in runtime:
        raise ValueError(f"{provider} frontend runtime provider pin mismatch")
    if f"KRW_AGENT_RELEASE_ARTIFACT_HASH={descriptor_hash}\n" not in runtime:
        raise ValueError(f"{provider} frontend runtime descriptor pin mismatch")
    if f"KRW_AGENT_RELEASE_SET_HASH={descriptor['release_set_hash']}\n" not in runtime:
        raise ValueError(f"{provider} frontend runtime release-set pin mismatch")
    manifest = json.loads((root / "release-manifest.json").read_text(encoding="utf-8"))
    return {
        "provider_id": provider,
        "physical_model": model_for_provider(provider),
        "manifest_hash": report["manifest_hash"],
        "descriptor_artifact_hash": descriptor_hash,
        "release_set_hash": descriptor["release_set_hash"],
        "git_commit": validate_commit(manifest.get("git_commit"), f"{provider}.git_commit"),
        "git_tree": validate_commit(manifest.get("git_tree"), f"{provider}.git_tree"),
    }


def validate_host_provenance(value: dict[str, Any], bundle: dict[str, Any]) -> dict[str, Any]:
    if value.get("package") != "@krw-agent/host" or value.get("version") != "0.1.0":
        raise ValueError("host package identity mismatch")
    commit = validate_commit(value.get("agent_git_commit"), "host package agent_git_commit")
    tree = validate_commit(value.get("agent_git_tree"), "host package agent_git_tree")
    if commit != bundle["git_commit"] or tree != bundle["git_tree"]:
        raise ValueError("host package provenance is bound to a different agent release")
    if not valid_hash(value.get("sha256")):
        raise ValueError("host package archive hash is invalid")
    return {
        "package": value["package"],
        "version": value["version"],
        "agent_git_commit": commit,
        "agent_git_tree": tree,
        "archive": value.get("archive"),
        "sha256": value["sha256"],
    }


def validate_acceptance(value: dict[str, Any], provider: str, bundle: dict[str, Any]) -> dict[str, Any]:
    if value.get("schema_version") != "krw-live-acceptance-run/v1" or value.get("status") != "pass":
        raise ValueError(f"{provider} live acceptance is not a pass")
    if value.get("provider_id") != provider or value.get("physical_model") != model_for_provider(provider):
        raise ValueError(f"{provider} live acceptance provider/model mismatch")
    if value.get("transport_failed") != 0:
        raise ValueError(f"{provider} live acceptance has transport failures")
    release = value.get("release")
    if not isinstance(release, dict):
        raise ValueError(f"{provider} live acceptance has no release binding")
    for key in ("manifest_hash", "descriptor_artifact_hash", "release_set_hash"):
        if release.get(key) != bundle[key]:
            raise ValueError(f"{provider} live acceptance {key} mismatch")
    report_hash = value.get("quality_report_hash")
    if not valid_hash(report_hash):
        raise ValueError(f"{provider} quality report hash is invalid")
    return {
        "evidence_hash": None,
        "quality_report_hash": report_hash,
        "completed": value.get("completed"),
        "manual_quality_reviews_required": value.get("manual_quality_reviews_required"),
        "session_groups": value.get("session_groups", {}),
    }


def validate_db(value: dict[str, Any]) -> dict[str, Any]:
    if value.get("ok") is not True and value.get("status") != "pass":
        raise ValueError("database compatibility evidence is not a pass")
    return {
        "status": "passed",
        "schema_compatibility": value.get("schema_compatibility"),
        "checked": value.get("checked"),
    }


def validate_canary(value: dict[str, Any], bundle: dict[str, Any]) -> dict[str, Any]:
    required_true = (
        "durable_final_projection",
        "non_empty_answer",
        "same_session_followup",
        "cross_session_isolation",
    )
    if value.get("status") != "pass" or any(value.get(key) is not True for key in required_true):
        raise ValueError("DeepSeek product canary is incomplete")
    for key in ("provider_id", "physical_model", "manifest_hash", "release_set_hash"):
        expected = {
            "provider_id": "deepseek",
            "physical_model": model_for_provider("deepseek"),
            "manifest_hash": bundle["manifest_hash"],
            "release_set_hash": bundle["release_set_hash"],
        }[key]
        if value.get(key) != expected:
            raise ValueError(f"DeepSeek canary {key} mismatch")
    return {"status": "passed", **{key: True for key in required_true}}


def write_index(path: pathlib.Path, value: dict[str, Any]) -> None:
    if not path.is_absolute() or path == pathlib.Path("/") or path.is_symlink():
        raise ValueError("evidence index output must be an absolute non-root path")
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = pathlib.Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(value, stream, ensure_ascii=False, sort_keys=True, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> int:
    args = parse_args()
    root = args.release_root
    if not root.is_absolute() or root.is_symlink() or not root.is_dir():
        raise ValueError("release root must be an absolute real directory")
    front_commit = validate_commit(args.front_commit, "front_commit")
    bundles = {provider: validate_sealed_bundle(root / provider, provider) for provider in PROVIDERS}
    if bundles["glm"]["git_commit"] != bundles["deepseek"]["git_commit"] or bundles["glm"]["git_tree"] != bundles["deepseek"]["git_tree"]:
        raise ValueError("provider bundles do not share one agent source identity")
    host_value, host_raw = bounded_json(args.host_provenance, "host provenance")
    db_value, db_raw = bounded_json(args.db_compatibility, "database compatibility")
    glm_value, glm_raw = bounded_json(args.glm_evidence, "GLM acceptance")
    deepseek_value, deepseek_raw = bounded_json(args.deepseek_evidence, "DeepSeek acceptance")
    canary_value, canary_raw = bounded_json(args.deepseek_canary, "DeepSeek canary")
    host = validate_host_provenance(host_value, bundles["deepseek"])
    database = validate_db(db_value)
    acceptance = {}
    for provider, value, raw in (("glm", glm_value, glm_raw), ("deepseek", deepseek_value, deepseek_raw)):
        acceptance[provider] = validate_acceptance(value, provider, bundles[provider])
        acceptance[provider]["evidence_hash"] = content_hash(raw)
    canary = validate_canary(canary_value, bundles["deepseek"])
    output = args.output or root.parent / f"{root.name}-production-evidence-index.json"
    output = output.resolve()
    try:
        output.relative_to(root)
    except ValueError:
        pass
    else:
        raise ValueError("evidence index must be outside the sealed release root")
    index = {
        "schema_version": "krw-production-evidence-index/v1",
        "agent_commit": bundles["deepseek"]["git_commit"],
        "agent_tree": bundles["deepseek"]["git_tree"],
        "front_commit": front_commit,
        "host_package": host,
        "bundles": bundles,
        "database_compatibility": database,
        "acceptance": acceptance,
        "deepseek_product_canary": canary,
        "source_evidence_hashes": {
            "host_provenance": content_hash(host_raw),
            "database_compatibility": content_hash(db_raw),
            "glm_acceptance": content_hash(glm_raw),
            "deepseek_acceptance": content_hash(deepseek_raw),
            "deepseek_canary": content_hash(canary_raw),
        },
    }
    write_index(output, index)
    print(json.dumps({"status": "pass", "output": str(output), "schema_version": index["schema_version"]}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, json.JSONDecodeError) as error:
        raise SystemExit(f"evidence index: {error}") from error
