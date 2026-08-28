#!/usr/bin/env python3
"""Offline contract checks for the two-provider release layout.

The test deliberately never contacts either provider and never starts a daemon.
It verifies that GLM and DeepSeek bundles use the closed provider/model mapping,
that the common artifacts are byte-identical, and that a staging directory is
not accidentally treated as a sealed production release.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import tempfile
import unittest

from release_provider import PROVIDER_MODELS, RELEASE_SCHEMA_VERSION
from verify_standalone_release import canonical_bytes, content_hash, verify_bundle


PROVIDERS = tuple(PROVIDER_MODELS)
PROVIDER_FILES = {
    "deployments/deployment-binding.yaml",
    "deployments/endpoint-registry.yaml",
    "deployments/model-registry.yaml",
    "release-manifest.json",
    "public-release.json",
    "release-authorization.json",
    "release-trust-registry.json",
    "frontend-runtime.env",
}
SEALED_FILES = {
    "public-release.json",
    "release-authorization.json",
    "release-trust-registry.json",
}

EXPECTED_TOOL_SESSION_REUSE = {
    "krw_ontology_query_context": "attested-stateless-v1",
    "krw_ontology_company_context": "attested-stateless-v1",
    "krw_market_snapshot": "attested-stateless-v1",
    "krw_ontology_query": "attested-stateless-v1",
    "krw_ontology_trace": "attested-stateless-v1",
    "krw_ontology_chain": "attested-stateless-v1",
    "krw_guru_query_context": "attested-stateless-v1",
    "krw_guru_company_brief": "attested-stateless-v1",
    "krw_guru_review_company_evidence": "attested-stateless-v1",
    "list_feed_items": "run-scoped",
    "get_feed_items": "run-scoped",
    "get_feed_context": "run-scoped",
    "search_catalog_filings": "run-scoped",
    "get_filing": "run-scoped",
    "get_filing_brief": "run-scoped",
    "list_filing_sections": "run-scoped",
    "read_filing_section": "run-scoped",
    "list_filing_documents": "run-scoped",
    "read_filing_document": "run-scoped",
    "get_form4_insider_transactions": "run-scoped",
}


def file_hash(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate_tool_session_matrix(binding_text: str) -> dict[str, str]:
    """Validate the closed MCP reuse policy without parsing provider data."""

    blocks = re.split(r"(?=^  - binding_key: )", binding_text, flags=re.MULTILINE)
    observed: dict[str, str] = {}
    for block in blocks:
        match = re.search(r"^  - binding_key: ([A-Za-z0-9._-]+)\s*$", block, re.MULTILINE)
        if not match:
            continue
        key = match.group(1)
        reuse = re.search(r"^    tool_session_reuse: ([A-Za-z0-9._-]+)\s*$", block, re.MULTILINE)
        if key in EXPECTED_TOOL_SESSION_REUSE:
            if reuse is None:
                raise ValueError(f"MCP binding has no reuse policy: {key}")
            observed[key] = reuse.group(1)
    missing = set(EXPECTED_TOOL_SESSION_REUSE) - set(observed)
    if missing:
        raise ValueError(f"MCP reuse matrix is missing bindings: {sorted(missing)}")
    mismatched = {
        key: (observed[key], expected)
        for key, expected in EXPECTED_TOOL_SESSION_REUSE.items()
        if observed[key] != expected
    }
    if mismatched:
        raise ValueError(f"MCP reuse matrix mismatch: {mismatched}")
    return observed


def write_fixture_bundle(root: pathlib.Path, provider: str) -> None:
    payload = root / "bin" / "krw-agent"
    payload.parent.mkdir(parents=True, exist_ok=True)
    payload.write_bytes(b"same common binary")
    registry = root / "deployments" / "model-registry.yaml"
    registry.parent.mkdir(parents=True, exist_ok=True)
    registry.write_text(f"provider: {provider}\nmodel: {PROVIDER_MODELS[provider]}\n")
    files = []
    for path in sorted(root.rglob("*")):
        if path.is_file():
            files.append(
                {
                    "path": path.relative_to(root).as_posix(),
                    "bytes": path.stat().st_size,
                    "content_hash": content_hash(path.read_bytes()),
                }
            )
    manifest = {
        "schema_version": RELEASE_SCHEMA_VERSION,
        "git_commit": "a" * 40,
        "git_tree": "b" * 40,
        "provider_id": provider,
        "physical_models": [PROVIDER_MODELS[provider]],
        "files": files,
    }
    manifest["manifest_hash"] = content_hash(canonical_bytes(manifest))
    (root / "release-manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )


def verify_dual_root(root: pathlib.Path, expect_sealed: bool) -> dict[str, object]:
    allowed_root_entries = set(PROVIDERS) | {"dual-release-index.json"}
    unexpected_root_entries = {
        entry.name for entry in root.iterdir() if entry.name not in allowed_root_entries
    }
    if unexpected_root_entries:
        raise ValueError("dual release root contains unexpected entries")
    reports = {
        provider: verify_bundle(root / provider) for provider in PROVIDERS
    }
    glm = root / "glm"
    deepseek = root / "deepseek"
    common_paths = set(
        path.relative_to(glm).as_posix()
        for path in glm.rglob("*")
        if path.is_file()
    ) & {
        path.relative_to(deepseek).as_posix()
        for path in deepseek.rglob("*")
        if path.is_file()
    }
    common_paths -= PROVIDER_FILES
    for relative in common_paths:
        if file_hash(glm / relative) != file_hash(deepseek / relative):
            raise ValueError(f"common artifact differs: {relative}")
    for provider in PROVIDERS:
        present = {
            path.relative_to(root / provider).as_posix()
            for path in (root / provider).rglob("*")
            if path.is_file()
        }
        if expect_sealed and not SEALED_FILES <= present:
            raise ValueError(f"sealed files missing for {provider}")
        if not expect_sealed and present & SEALED_FILES:
            raise ValueError(f"unsigned bundle contains sealed files for {provider}")
    return reports


class DualProviderReleaseTest(unittest.TestCase):
    def test_provider_models_are_closed(self) -> None:
        self.assertEqual(PROVIDER_MODELS, {
            "glm": "glm-5.3-flash",
            "deepseek": "deepseek-v4-flash",
        })

    def test_source_registries_require_an_explicit_provider(self) -> None:
        for environment in ("local", "prod"):
            deployment_root = pathlib.Path("deployments") / environment
            self.assertFalse((deployment_root / "model-registry.yaml").exists())
            self.assertTrue((deployment_root / "model-registry.glm.yaml").is_file())
            self.assertTrue((deployment_root / "model-registry.deepseek.yaml").is_file())

    def test_fixture_dual_root_is_verified_and_common_bytes_match(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for provider in PROVIDERS:
                write_fixture_bundle(root / provider, provider)
            reports = verify_dual_root(root, expect_sealed=False)
            self.assertEqual(reports["glm"]["provider_id"], "glm")
            self.assertEqual(reports["deepseek"]["provider_id"], "deepseek")

    def test_mixed_provider_manifest_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            write_fixture_bundle(root / "deepseek", "deepseek")
            manifest_path = root / "deepseek" / "release-manifest.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["provider_id"] = "glm"
            manifest["manifest_hash"] = content_hash(
                canonical_bytes({
                    key: value for key, value in manifest.items()
                    if key != "manifest_hash"
                })
            )
            manifest_path.write_text(
                json.dumps(manifest, sort_keys=True, indent=2) + "\n",
                encoding="utf-8",
            )
            with self.assertRaises(ValueError):
                verify_bundle(root / "deepseek")

    def test_common_staging_directory_is_not_a_release_root_entry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for provider in PROVIDERS:
                write_fixture_bundle(root / provider, provider)
            (root / "common").mkdir()
            with self.assertRaisesRegex(ValueError, "unexpected entries"):
                verify_dual_root(root, expect_sealed=False)

    def test_checked_in_mcp_reuse_matrix_is_closed_and_provider_independent(self) -> None:
        binding = pathlib.Path("deployments/prod/deployment-binding.example.yaml").read_text(
            encoding="utf-8"
        )
        observed = validate_tool_session_matrix(binding)
        self.assertEqual(observed["krw_ontology_query"], "attested-stateless-v1")
        self.assertEqual(observed["krw_guru_company_brief"], "attested-stateless-v1")
        self.assertEqual(observed["get_feed_context"], "run-scoped")
        self.assertEqual(observed["read_filing_document"], "run-scoped")

    def test_reuse_matrix_rejects_accidental_feed_sharing(self) -> None:
        binding = pathlib.Path("deployments/prod/deployment-binding.example.yaml").read_text(
            encoding="utf-8"
        ).replace(
            "binding_key: get_feed_context\n    mcp_tool_name: get_feed_context\n    endpoint_ref: krw-feed-local\n    credential_ref: KRW_FEED_MCP_TOKEN\n    auth_scope: tenant\n    tool_session_reuse: run-scoped",
            "binding_key: get_feed_context\n    mcp_tool_name: get_feed_context\n    endpoint_ref: krw-feed-local\n    credential_ref: KRW_FEED_MCP_TOKEN\n    auth_scope: tenant\n    tool_session_reuse: attested-stateless-v1",
        )
        with self.assertRaisesRegex(ValueError, "MCP reuse matrix mismatch"):
            validate_tool_session_matrix(binding)

    def test_capability_installer_uses_sealed_origin_and_session_policy(self) -> None:
        installer = pathlib.Path(
            "packaging/launchd/install-local-mac-capabilityd-release.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("endpoint-registry.yaml", installer)
        self.assertIn("tool_session_reuse", installer)
        self.assertIn('"allowedOrigins": [origin]', installer)
        self.assertNotIn('"https://krw-agent.local"', installer)
        self.assertNotIn('"toolSessionReuse": "attested-stateless-v1"', installer)
        self.assertIn('"component":"krw-capabilityd"', installer)
        self.assertIn("write_activation_status failed", installer)
        self.assertNotIn("rollback)", installer)
        self.assertNotIn("plist.previous", installer)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--staging-root", type=pathlib.Path)
    parser.add_argument(
        "--expect-sealed",
        choices=("true", "false"),
        default="false",
    )
    args = parser.parse_args()
    if args.staging_root:
        verify_dual_root(args.staging_root, args.expect_sealed == "true")
        print(json.dumps({"status": "verified", "root": str(args.staging_root)}))
        return 0
    result = unittest.main(argv=[__file__], exit=False)
    return 0 if result.result.wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
