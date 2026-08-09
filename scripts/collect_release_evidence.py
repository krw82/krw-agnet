#!/usr/bin/env python3
"""Execute the fixed KRW release matrix and emit content-addressed evidence.

This program deliberately accepts no arbitrary command. It runs a reviewed,
fixed command matrix with credential-shaped environment variables removed.
Long-running performance evidence and external live/rotation receipts remain
explicitly separate from local correctness evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time
from typing import Any


SCHEMA_VERSION = "krw-release-evidence/v1"
LIVE_SCHEMA_VERSION = "krw-live-acceptance/v1"
ROTATION_SCHEMA_VERSION = "krw-credential-rotation/v1"
MODEL_ID = "glm-5.2"
MAX_RECEIPT_BYTES = 1024 * 1024
MAX_LOCAL_ARTIFACT_BYTES = 64 * 1024 * 1024
SOURCE_FILES = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rustfmt.toml")
SOURCE_DIRS = (
    "agents",
    "bins",
    "contracts",
    "crates",
    "deployments",
    "docs",
    "evals",
    "fixtures",
    "migrations",
    "packages",
    "perf",
    "scripts",
)
SECRET_ENV_MARKERS = ("KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL")


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")


def content_hash(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def safe_environment() -> dict[str, str]:
    environment = dict(os.environ)
    for name in list(environment):
        upper = name.upper()
        provider_scoped = upper.startswith(
            ("DEEPSEEK_", "ANTHROPIC_", "OPENAI_", "SUPABASE_")
        )
        krw_external = upper.startswith("KRW_") and upper.endswith(
            ("_URL", "_ENDPOINT", "_BASE")
        )
        if (
            any(marker in upper for marker in SECRET_ENV_MARKERS)
            or provider_scoped
            or krw_external
        ):
            del environment[name]
    environment["PATH"] = "/opt/homebrew/opt/rustup/bin:" + environment.get("PATH", "")
    environment["CARGO_TERM_COLOR"] = "never"
    environment["KRW_AGENT_OFFLINE_RELEASE_TESTS"] = "1"
    return environment


def iter_source_files(root: pathlib.Path) -> list[pathlib.Path]:
    selected: list[pathlib.Path] = []
    for name in SOURCE_FILES:
        path = root / name
        if path.is_file() and not path.is_symlink():
            selected.append(path)
    for name in SOURCE_DIRS:
        directory = root / name
        if not directory.is_dir() or directory.is_symlink():
            continue
        for path in directory.rglob("*"):
            if (
                path.is_file()
                and not path.is_symlink()
                and ".DS_Store" not in path.parts
                and "node_modules" not in path.parts
                and "target" not in path.parts
                and ".ruff_cache" not in path.parts
                and "__pycache__" not in path.parts
            ):
                selected.append(path)
    return sorted(set(selected), key=lambda path: path.relative_to(root).as_posix())


def source_tree_hash(root: pathlib.Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    files = iter_source_files(root)
    for path in files:
        relative = path.relative_to(root).as_posix().encode("utf-8")
        data = path.read_bytes()
        digest.update(relative)
        digest.update(b"\0")
        digest.update(hashlib.sha256(data).digest())
        digest.update(b"\0")
    return "sha256:" + digest.hexdigest(), len(files)


def git_identity(root: pathlib.Path) -> dict[str, Any]:
    def git(*arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *arguments],
            cwd=root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )

    head = git("rev-parse", "HEAD")
    status = git("status", "--porcelain=v1", "--untracked-files=normal")
    return {
        "head": head.stdout.strip() if head.returncode == 0 else None,
        "clean": head.returncode == 0 and status.returncode == 0 and not status.stdout,
        "status_hash": content_hash(status.stdout.encode("utf-8")),
    }


def load_receipt(path: pathlib.Path, expected_schema: str) -> tuple[dict[str, Any], str]:
    if not path.is_file() or path.is_symlink() or path.stat().st_size > MAX_RECEIPT_BYTES:
        raise ValueError("receipt must be a bounded regular file")
    raw = path.read_bytes()
    value = json.loads(raw)
    if not isinstance(value, dict) or value.get("schema_version") != expected_schema:
        raise ValueError(f"receipt schema must be {expected_schema}")
    return value, content_hash(canonical_bytes(value))


def validate_rotation_receipt(value: dict[str, Any]) -> None:
    required_true = (
        "old_credential_revoked",
        "redacted_repository_scan_passed",
        "redacted_log_scan_passed",
    )
    if any(value.get(field) is not True for field in required_true):
        raise ValueError("credential rotation receipt is incomplete")
    if value.get("contains_secret_material") is not False:
        raise ValueError("credential rotation receipt must contain no secret material")
    for field in ("new_secret_version_hash", "repository_scan_hash", "log_scan_hash"):
        observed = value.get(field)
        if not isinstance(observed, str) or not valid_hash(observed):
            raise ValueError(f"credential rotation receipt has invalid {field}")


def validate_live_receipt(value: dict[str, Any], release_set_hash: str) -> None:
    if value.get("status") != "pass" or value.get("redacted") is not True:
        raise ValueError("live acceptance receipt is not a redacted pass")
    if value.get("requested_model") != MODEL_ID or value.get("observed_model") != MODEL_ID:
        raise ValueError("live acceptance must request and observe exact DeepSeek Flash")
    if value.get("release_set_hash") != release_set_hash:
        raise ValueError("live acceptance is bound to a different release set")
    for field in (
        "data_release_hash",
        "provider_contract_hash",
        "mcp_contract_hash",
        "postgres_fault_matrix_hash",
        "quality_report_hash",
    ):
        observed = value.get(field)
        if not isinstance(observed, str) or not valid_hash(observed):
            raise ValueError(f"live acceptance receipt has invalid {field}")


def valid_hash(value: str) -> bool:
    if not value.startswith("sha256:") or len(value) != 71:
        return False
    return all(character in "0123456789abcdef" for character in value[7:])


def load_release_set_hash(path: pathlib.Path) -> tuple[str, str]:
    if not path.is_file() or path.is_symlink() or path.stat().st_size > MAX_RECEIPT_BYTES:
        raise ValueError("public release descriptor must be a bounded regular file")
    value = json.loads(path.read_bytes())
    if not isinstance(value, dict) or value.get("schema_version") != 2:
        raise ValueError("public release descriptor schema must be 2")
    descriptor_hash = content_hash(canonical_bytes(value))
    release_hash = value.get("release_set_hash")
    if not isinstance(release_hash, str) or not valid_hash(release_hash):
        raise ValueError("public release descriptor has invalid release_set_hash")
    return release_hash, descriptor_hash


def fixed_steps(
    profile: str,
    work_dir: pathlib.Path,
    release_verification: tuple[pathlib.Path, pathlib.Path, pathlib.Path, str, str] | None,
) -> list[tuple[str, list[str]]]:
    semantic_report = work_dir / "semantic_report.json"
    steps = [
        (
            "cargo_dependency_graph",
            ["cargo", "metadata", "--locked", "--format-version", "1"],
        ),
        (
            "host_dependency_graph",
            ["npm", "--prefix", "packages/host-ts", "ls", "--all", "--json"],
        ),
        ("rustfmt", ["cargo", "fmt", "--all", "--", "--check"]),
        (
            "standalone_manifest_verifier",
            ["python3", "scripts/test_standalone_release_manifest.py"],
        ),
        ("workspace_tests", ["cargo", "test", "--workspace", "--no-fail-fast"]),
        (
            "workspace_clippy",
            ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
        ),
        ("agent_images", ["bash", "agents/check-all.sh"]),
        (
            "semantic_evals",
            [
                "python3",
                "scripts/validate_semantic_evals.py",
                "--suite",
                "evals/krw-semantic/v2",
                "--report",
                str(semantic_report),
            ],
        ),
        ("host_typecheck", ["npm", "--prefix", "packages/host-ts", "run", "typecheck"]),
        ("host_tests", ["npm", "--prefix", "packages/host-ts", "test"]),
        ("postgres_faults", ["bash", "scripts/test-session-memory-postgres.sh"]),
        ("performance", ["bash", "scripts/run-performance-gates.sh", profile]),
    ]
    if release_verification is not None:
        descriptor, authorization, trust_registry, runtime_version, kernel_version = (
            release_verification
        )
        steps.append(
            (
                "release_authorization",
                [
                    "cargo",
                    "run",
                    "--locked",
                    "-p",
                    "krw-agent",
                    "--",
                    "release",
                    "verify",
                    "--descriptor",
                    str(descriptor),
                    "--authorization",
                    str(authorization),
                    "--trust-registry",
                    str(trust_registry),
                    "--runtime-version",
                    runtime_version,
                    "--kernel-version",
                    kernel_version,
                ],
            )
        )
    return steps


def execute_step(
    root: pathlib.Path,
    evidence_dir: pathlib.Path,
    environment: dict[str, str],
    step_id: str,
    command: list[str],
) -> dict[str, Any]:
    log_path = evidence_dir / f"{step_id}.log"
    started = time.monotonic()
    with log_path.open("wb") as log:
        process = subprocess.run(
            command,
            cwd=root,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            check=False,
        )
        log.flush()
        os.fsync(log.fileno())
    elapsed_ms = round((time.monotonic() - started) * 1000)
    log_hash = content_hash(log_path.read_bytes())
    return {
        "id": step_id,
        "command": command,
        "status": "pass" if process.returncode == 0 else "fail",
        "exit_code": process.returncode,
        "duration_ms": elapsed_ms,
        "log_hash": log_hash,
        "log_file": log_path.name,
    }


def atomic_write(path: pathlib.Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    temporary = pathlib.Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def collect_local_artifacts(
    root: pathlib.Path, staging: pathlib.Path, profile: str
) -> dict[str, dict[str, Any]]:
    candidates = {
        "semantic_report": staging / "semantic_report.json",
        "performance_report": root / "target" / "perf" / f"{profile}-report.json",
    }
    artifacts: dict[str, dict[str, Any]] = {}
    for artifact_id, source in candidates.items():
        if not source.is_file() or source.is_symlink():
            continue
        size = source.stat().st_size
        if size <= 0 or size > MAX_LOCAL_ARTIFACT_BYTES:
            raise ValueError(f"local artifact {artifact_id} exceeds its byte bound")
        data = source.read_bytes()
        value = json.loads(data)
        if not isinstance(value, dict) or value.get("status") != "pass":
            raise ValueError(f"local artifact {artifact_id} is not a passing report")
        if artifact_id == "semantic_report" and value.get("schema_version") != "krw-semantic-report/v1":
            raise ValueError("semantic report schema is invalid")
        if artifact_id == "performance_report":
            if value.get("profile") != profile:
                raise ValueError("performance report profile mismatch")
            authoritative_profile = profile != "ci"
            if value.get("performance_authority") is not authoritative_profile:
                raise ValueError("performance authority does not match the selected profile")
            if authoritative_profile and value.get("complete_performance_evidence") is not True:
                raise ValueError("long performance report is incomplete")
        destination = staging / f"{artifact_id}.json"
        if source != destination:
            atomic_write(destination, data)
        artifacts[artifact_id] = {
            "file": destination.name,
            "bytes": size,
            "content_hash": content_hash(data),
        }
    return artifacts


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--profile", choices=("ci", "release-4gb", "production-7d"), default="ci"
    )
    parser.add_argument("--output-dir", type=pathlib.Path)
    parser.add_argument("--release-descriptor", type=pathlib.Path)
    parser.add_argument("--release-authorization", type=pathlib.Path)
    parser.add_argument("--release-trust-registry", type=pathlib.Path)
    parser.add_argument("--runtime-version")
    parser.add_argument("--kernel-version")
    parser.add_argument("--credential-rotation-receipt", type=pathlib.Path)
    parser.add_argument("--live-acceptance-receipt", type=pathlib.Path)
    parser.add_argument("--require-authoritative", action="store_true")
    parser.add_argument("--verify-manifest", type=pathlib.Path)
    return parser.parse_args()


def verify_evidence_manifest(path: pathlib.Path) -> dict[str, Any]:
    value, _ = load_receipt(path, SCHEMA_VERSION)
    declared_hash = value.pop("manifest_hash", None)
    if not isinstance(declared_hash, str) or content_hash(canonical_bytes(value)) != declared_hash:
        raise ValueError("release evidence manifest hash mismatch")
    value["manifest_hash"] = declared_hash
    directory = path.resolve().parent
    steps = value.get("steps")
    if not isinstance(steps, list) or not steps:
        raise ValueError("release evidence has no executed steps")
    for step in steps:
        if not isinstance(step, dict):
            raise ValueError("release evidence step is invalid")
        verify_local_file(directory, step.get("log_file"), step.get("log_hash"), None)
    artifacts = value.get("local_artifacts")
    if not isinstance(artifacts, dict):
        raise ValueError("release evidence artifacts are invalid")
    for artifact in artifacts.values():
        if not isinstance(artifact, dict):
            raise ValueError("release evidence artifact is invalid")
        verify_local_file(
            directory,
            artifact.get("file"),
            artifact.get("content_hash"),
            artifact.get("bytes"),
        )
    authoritative = value.get("authoritative") is True
    expected_authoritative = (
        value.get("status") == "pass"
        and value.get("profile") != "ci"
        and value.get("external_valid") is True
        and isinstance(value.get("source"), dict)
        and isinstance(value["source"].get("git"), dict)
        and value["source"]["git"].get("clean") is True
        and all(step.get("status") == "pass" for step in steps)
        and set(artifacts) == {"semantic_report", "performance_report"}
        and isinstance(value.get("external"), dict)
        and valid_hash(value["external"].get("release_authorization_hash", ""))
        and valid_hash(value["external"].get("release_trust_registry_hash", ""))
        and any(step.get("id") == "release_authorization" for step in steps)
    )
    if authoritative != expected_authoritative:
        raise ValueError("release evidence authority derivation is inconsistent")
    return value


def verify_local_file(
    directory: pathlib.Path, name: Any, expected_hash: Any, expected_size: Any
) -> None:
    if not isinstance(name, str) or pathlib.Path(name).name != name:
        raise ValueError("evidence file name is unsafe")
    if not isinstance(expected_hash, str) or not valid_hash(expected_hash):
        raise ValueError("evidence file hash is invalid")
    path = directory / name
    if not path.is_file() or path.is_symlink() or path.stat().st_size > MAX_LOCAL_ARTIFACT_BYTES:
        raise ValueError("evidence file is missing or unsafe")
    data = path.read_bytes()
    if content_hash(data) != expected_hash:
        raise ValueError("evidence file content hash mismatch")
    if expected_size is not None and expected_size != len(data):
        raise ValueError("evidence file size mismatch")


def main() -> int:
    args = parse_args()
    if args.verify_manifest is not None:
        try:
            manifest = verify_evidence_manifest(args.verify_manifest.resolve())
        except (OSError, ValueError, json.JSONDecodeError) as error:
            print(content_hash(str(error).encode("utf-8")), file=sys.stderr)
            return 1
        print(
            json.dumps(
                {
                    "status": "verified",
                    "manifest_hash": manifest["manifest_hash"],
                    "authoritative": manifest["authoritative"],
                },
                ensure_ascii=False,
                indent=2,
            )
        )
        return 0
    root = pathlib.Path(__file__).resolve().parent.parent
    default_name = f"{args.profile}-{time.time_ns()}"
    output_dir = (
        args.output_dir or root / "target" / "release-evidence" / default_name
    ).resolve()
    if output_dir == root or root not in output_dir.parents:
        raise SystemExit("output directory must be a non-root path inside this repository")
    if output_dir.exists() or output_dir.is_symlink():
        raise SystemExit("output directory must not already exist")
    output_dir.parent.mkdir(parents=True, exist_ok=True)
    staging = pathlib.Path(
        tempfile.mkdtemp(prefix=f".{output_dir.name}.", dir=output_dir.parent)
    )

    source_hash, source_files = source_tree_hash(root)
    git = git_identity(root)
    environment = safe_environment()
    release_paths = (
        args.release_descriptor,
        args.release_authorization,
        args.release_trust_registry,
        args.runtime_version,
        args.kernel_version,
    )
    if any(value is not None for value in release_paths) and not all(release_paths):
        raise SystemExit(
            "release descriptor, authorization, trust registry, runtime version, and kernel version must be supplied together"
        )
    release_verification = (
        (
            args.release_descriptor.resolve(),
            args.release_authorization.resolve(),
            args.release_trust_registry.resolve(),
            args.runtime_version,
            args.kernel_version,
        )
        if all(release_paths)
        else None
    )
    fixed_matrix = fixed_steps(args.profile, staging, release_verification)
    steps: list[dict[str, Any]] = []
    for step_id, command in fixed_matrix:
        result = execute_step(root, staging, environment, step_id, command)
        steps.append(result)
        if result["status"] != "pass":
            break

    local_pass = len(steps) == len(fixed_matrix) and all(
        step["status"] == "pass" for step in steps
    )
    local_artifacts: dict[str, dict[str, Any]] = {}
    local_artifact_error_hash = None
    if local_pass:
        try:
            local_artifacts = collect_local_artifacts(root, staging, args.profile)
            if set(local_artifacts) != {"semantic_report", "performance_report"}:
                raise ValueError("required local report artifact is missing")
        except (OSError, ValueError, json.JSONDecodeError) as error:
            local_pass = False
            local_artifact_error_hash = content_hash(str(error).encode("utf-8"))
    external: dict[str, Any] = {
        "release_descriptor_hash": None,
        "release_authorization_hash": None,
        "release_trust_registry_hash": None,
        "credential_rotation_receipt_hash": None,
        "live_acceptance_receipt_hash": None,
    }
    external_valid = False
    external_error_hash = None
    try:
        if (
            args.release_descriptor
            and args.release_authorization
            and args.release_trust_registry
            and args.credential_rotation_receipt
            and args.live_acceptance_receipt
        ):
            release_hash, descriptor_hash = load_release_set_hash(args.release_descriptor.resolve())
            authorization_hash = bounded_regular_file_hash(
                args.release_authorization.resolve(), MAX_RECEIPT_BYTES
            )
            trust_registry_hash = bounded_regular_file_hash(
                args.release_trust_registry.resolve(), MAX_RECEIPT_BYTES
            )
            rotation, rotation_hash = load_receipt(
                args.credential_rotation_receipt.resolve(), ROTATION_SCHEMA_VERSION
            )
            live, live_hash = load_receipt(
                args.live_acceptance_receipt.resolve(), LIVE_SCHEMA_VERSION
            )
            validate_rotation_receipt(rotation)
            validate_live_receipt(live, release_hash)
            external = {
                "release_descriptor_hash": descriptor_hash,
                "release_authorization_hash": authorization_hash,
                "release_trust_registry_hash": trust_registry_hash,
                "credential_rotation_receipt_hash": rotation_hash,
                "live_acceptance_receipt_hash": live_hash,
            }
            external_valid = True
    except (OSError, ValueError, json.JSONDecodeError) as error:
        external_error_hash = content_hash(str(error).encode("utf-8"))

    authoritative = (
        local_pass
        and args.profile != "ci"
        and git["clean"]
        and external_valid
    )
    status = "fail" if not local_pass else ("pass" if authoritative else "incomplete")
    manifest: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "profile": args.profile,
        "status": status,
        "authoritative": authoritative,
        "model_policy": {"physical_models": [MODEL_ID], "aliases_allowed": False},
        "source": {
            "tree_hash": source_hash,
            "file_count": source_files,
            "git": git,
        },
        "credential_environment_removed": True,
        "steps": steps,
        "local_artifacts": local_artifacts,
        "local_artifact_error_hash": local_artifact_error_hash,
        "external": external,
        "external_valid": external_valid,
        "external_error_hash": external_error_hash,
        "limitations": [
            "ci is never authoritative",
            "a dirty or untracked Git source tree is never authoritative",
            "live and credential-rotation evidence must be supplied as bounded redacted receipts",
            "a signed release authorization and public trust registry must verify for the exact descriptor",
            "performance authority is limited to the selected wall-clock profile",
        ],
    }
    manifest["manifest_hash"] = content_hash(canonical_bytes(manifest))
    atomic_write(staging / "manifest.json", json.dumps(manifest, ensure_ascii=False, indent=2).encode("utf-8"))
    os.replace(staging, output_dir)
    print(json.dumps(manifest, ensure_ascii=False, indent=2))
    if not local_pass or (args.require_authoritative and not authoritative):
        return 1
    return 0


def bounded_regular_file_hash(path: pathlib.Path, max_bytes: int) -> str:
    if not path.is_file() or path.is_symlink() or path.stat().st_size <= 0 or path.stat().st_size > max_bytes:
        raise ValueError("release authorization artifact is missing or unsafe")
    return content_hash(path.read_bytes())


if __name__ == "__main__":
    sys.exit(main())
