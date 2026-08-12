#!/usr/bin/env python3
"""Run the same bounded quality corpus against one sealed provider bundle.

The command intentionally reuses the already-running Gateway. It never starts
PostgreSQL, a provider daemon, or the ontology MCP service. The operator starts
that stack with the selected provider, then runs this command once for GLM and
once for DeepSeek. It verifies the selected bundle before any question is sent
and writes only a redacted provider/release envelope next to the private matrix
report.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys
from typing import Any

from release_provider import PROVIDER_MODELS, model_for_provider, validate_public_descriptor
from verify_standalone_release import verify_bundle


HASH_RE = "sha256:"
MAX_DESCRIPTOR_BYTES = 16 * 1024 * 1024


def content_hash(raw: bytes) -> str:
    return HASH_RE + hashlib.sha256(raw).hexdigest()


def parse_args() -> argparse.Namespace:
    root = pathlib.Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provider", choices=tuple(sorted(PROVIDER_MODELS)), required=True)
    parser.add_argument("--release", type=pathlib.Path, required=True)
    parser.add_argument(
        "--corpus",
        type=pathlib.Path,
        default=root / "fixtures/live-quality/v1/dual-provider-production-v1.json",
    )
    parser.add_argument("--gateway-url", default=os.environ.get("KRW_AGENT_GATEWAY_URL", "http://127.0.0.1:4318/v1/agent"))
    parser.add_argument("--report-dir", type=pathlib.Path, required=True)
    parser.add_argument("--parallelism", type=int, default=2)
    parser.add_argument("--timeout-seconds", type=int, default=720)
    parser.add_argument("--poll-seconds", type=float, default=2.0)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--require-sealed", action="store_true")
    return parser.parse_args()


def valid_hash(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 71
        and value.startswith(HASH_RE)
        and all(character in "0123456789abcdef" for character in value[7:])
    )


def load_descriptor(path: pathlib.Path) -> tuple[dict[str, Any], str]:
    if not path.is_file() or path.is_symlink() or path.stat().st_size <= 0 or path.stat().st_size > MAX_DESCRIPTOR_BYTES:
        raise ValueError("provider release descriptor is missing or unsafe")
    raw = path.read_bytes()
    value = json.loads(raw)
    if not isinstance(value, dict) or not valid_hash(value.get("release_set_hash")):
        raise ValueError("provider release descriptor has no valid release_set_hash")
    return value, content_hash(raw)


def verify_provider_release(root: pathlib.Path, provider: str, require_sealed: bool) -> dict[str, Any]:
    if not root.is_absolute() or root.is_symlink() or not root.is_dir():
        raise ValueError("release must be an absolute real directory")
    report = verify_bundle(root)
    if report["provider_id"] != provider or report["physical_models"] != [model_for_provider(provider)]:
        raise ValueError("release provider/model does not match the selected acceptance lane")
    descriptor, descriptor_hash = load_descriptor(root / "public-release.json")
    validate_public_descriptor(
        (root / "public-release.json").resolve(),
        provider,
        expected_artifact_hash=descriptor_hash,
        expected_release_set_hash=descriptor["release_set_hash"],
    )
    if require_sealed:
        for name in ("release-authorization.json", "release-trust-registry.json", "frontend-runtime.env"):
            path = root / name
            if not path.is_file() or path.is_symlink():
                raise ValueError(f"sealed release is missing {name}")
        runtime_env = (root / "frontend-runtime.env").read_text(encoding="utf-8")
        if f"KRW_AGENT_PROVIDER={provider}\n" not in runtime_env:
            raise ValueError("frontend runtime provider pin does not match the acceptance lane")
    return {
        "provider_id": provider,
        "physical_model": model_for_provider(provider),
        "manifest_hash": report["manifest_hash"],
        "descriptor_artifact_hash": descriptor_hash,
        "release_set_hash": descriptor["release_set_hash"],
    }


def load_json(path: pathlib.Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("quality report must be an object")
    return value


def validate_session_groups(report: dict[str, Any], *, live: bool) -> dict[str, str]:
    """Require each grouped follow-up to stay in one returned chat session."""
    results = report.get("results")
    if not isinstance(results, list):
        raise ValueError("quality report has no results list")
    sessions: dict[str, set[str]] = {}
    for result in results:
        if not isinstance(result, dict):
            raise ValueError("quality report contains an invalid result")
        group = result.get("session_group")
        if group is None:
            continue
        if not isinstance(group, str) or not group:
            raise ValueError("quality report has an invalid session group")
        trace = result.get("execution_trace")
        session = trace.get("session_id") if isinstance(trace, dict) else None
        if live:
            if not isinstance(session, str) or not session:
                raise ValueError(f"quality session group has no session: {group}")
            sessions.setdefault(group, set()).add(session)
    if any(len(values) != 1 for values in sessions.values()):
        raise ValueError("quality session group crossed chat sessions")
    return {group: next(iter(values)) for group, values in sorted(sessions.items())}


def write_json(path: pathlib.Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    path.chmod(0o600)


def main() -> int:
    args = parse_args()
    if not args.report_dir.is_absolute() or args.report_dir.exists() or args.report_dir.is_symlink():
        raise SystemExit("--report-dir must be an absolute path that does not already exist")
    if args.parallelism < 1 or args.parallelism > 4:
        raise SystemExit("--parallelism must be in 1..4")
    release = verify_provider_release(args.release, args.provider, args.require_sealed)
    command = [
        sys.executable,
        str(pathlib.Path(__file__).with_name("run_live_quality_matrix.py")),
        "--corpus",
        str(args.corpus.resolve()),
        "--per-bucket",
        "3",
        "--parallelism",
        str(args.parallelism),
        "--timeout-seconds",
        str(args.timeout_seconds),
        "--poll-seconds",
        str(args.poll_seconds),
        "--gateway-url",
        args.gateway_url,
        "--report-dir",
        str(args.report_dir.resolve()),
    ]
    if args.dry_run:
        command.append("--dry-run")
    environment = dict(os.environ)
    environment["KRW_AGENT_PROVIDER"] = args.provider
    environment["KRW_AGENT_RELEASE_DESCRIPTOR_PATH"] = str(args.release.absolute() / "public-release.json")
    environment["KRW_AGENT_RELEASE_ARTIFACT_HASH"] = release["descriptor_artifact_hash"]
    environment["KRW_AGENT_RELEASE_SET_HASH"] = release["release_set_hash"]
    completed = subprocess.run(command, env=environment, check=False)
    report_path = args.report_dir / "report.json"
    if not report_path.is_file():
        raise SystemExit("quality runner did not produce report.json")
    report = load_json(report_path)
    grouped_sessions = validate_session_groups(report, live=not args.dry_run)
    summary = report.get("summary") if isinstance(report.get("summary"), dict) else {}
    transport_failed = summary.get("transport_failed")
    evidence = {
        "schema_version": "krw-live-acceptance-run/v1",
        "status": "pass" if completed.returncode == 0 and transport_failed == 0 else "fail",
        "provider_id": args.provider,
        "physical_model": model_for_provider(args.provider),
        "release": release,
        "corpus": {
            "path": str(args.corpus.resolve()),
            "suite_id": report.get("corpus", {}).get("suite_id"),
            "suite_version": report.get("corpus", {}).get("suite_version"),
        },
        "quality_report_hash": content_hash(report_path.read_bytes()),
        "transport_failed": transport_failed,
        "completed": summary.get("completed"),
        "manual_quality_reviews_required": summary.get("manual_quality_reviews_required"),
        "session_groups": grouped_sessions,
        "dry_run": args.dry_run,
    }
    write_json(args.report_dir / "provider-release-evidence.json", evidence)
    print(json.dumps(evidence, ensure_ascii=False, indent=2))
    return completed.returncode


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(content_hash(str(error).encode("utf-8")), file=sys.stderr)
        raise SystemExit(2) from error
