#!/usr/bin/env python3
"""Behavioral regression tests for forward-only local launchd activation."""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import tarfile
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
AGENT_INSTALLER = ROOT / "packaging/launchd/install-local-mac-agentd-release.sh"
CAPABILITY_INSTALLER = ROOT / "packaging/launchd/install-local-mac-capabilityd-release.sh"
EMPTY_SHA256 = "sha256:" + "0" * 64


def write(path: pathlib.Path, value: str, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value, encoding="utf-8")
    path.chmod(mode)


def install_command_stubs(root: pathlib.Path) -> pathlib.Path:
    bin_dir = root / "bin"
    write(
        bin_dir / "launchctl",
        """#!/bin/sh
printf '%s\n' "$*" >> "$KRW_TEST_LAUNCHCTL_LOG"
[ "${1:-}" != bootstrap ]
""",
        0o755,
    )
    write(bin_dir / "plutil", "#!/bin/sh\nexit 0\n", 0o755)
    write(bin_dir / "curl", "#!/bin/sh\nexit 22\n", 0o755)
    write(
        bin_dir / "openssl",
        "#!/bin/sh\nprintf '%064d\n' 0\n",
        0o755,
    )
    return bin_dir


def make_dual_release(root: pathlib.Path) -> None:
    bundles: dict[str, dict[str, str]] = {}
    for provider, model in (("glm", "glm-5.3-flash"), ("deepseek", "deepseek-v4-flash")):
        bundle = root / provider
        manifest = {
            "provider_id": provider,
            "physical_models": [model],
            "manifest_hash": f"manifest-{provider}",
            "git_commit": "commit",
            "git_tree": "tree",
        }
        descriptor = {
            "schema_version": 3,
            "release_set_hash": EMPTY_SHA256,
            "entries": [{"execution": {"resolved_model": model}}],
        }
        write(bundle / "release-manifest.json", json.dumps(manifest))
        write(bundle / "public-release.json", json.dumps(descriptor))
        descriptor_hash = "sha256:" + __import__("hashlib").sha256(
            (bundle / "public-release.json").read_bytes()
        ).hexdigest()
        write(
            bundle / "frontend-runtime.env",
            "".join(
                (
                    f"KRW_AGENT_PROVIDER={provider}\n",
                    "KRW_AGENT_RELEASE_DESCRIPTOR_PATH=/run/krw-agent/public-release.json\n",
                    f"KRW_AGENT_RELEASE_ARTIFACT_HASH={descriptor_hash}\n",
                    f"KRW_AGENT_RELEASE_SET_HASH={EMPTY_SHA256}\n",
                    "KRW_RUNTIME_ENVIRONMENT=prod\n",
                )
            ),
        )
        write(bundle / "packaging/verify_standalone_release.py", "raise SystemExit(0)\n")
        required = (
            "bin/krw-agent",
            "bin/krw-agentd",
            "packaging/launchd/krw-agentd-start-local",
            "packaging/launchd/install-local-mac-agentd-release.sh",
            "packaging/launchd/krw-capabilityd-start-local",
            "packaging/launchd/install-local-mac-capabilityd-release.sh",
            "packaging/local-mcp-gateways/mcp_tls_proxy.mjs",
            "packaging/systemd/krw-agentd-start",
            "capability-runtime/identity.json",
            "capability-runtime/capability-runtime.venv.tar.gz",
            "release-authorization.json",
            "release-trust-registry.json",
        )
        for name in required:
            write(bundle / name, "placeholder\n", 0o755 if "start" in name or "install" in name else 0o644)
        bundles[provider] = {
            "provider_id": provider,
            "physical_model": model,
            "manifest_hash": manifest["manifest_hash"],
            "descriptor_artifact_hash": descriptor_hash,
            "release_set_hash": EMPTY_SHA256,
        }
    write(
        root / "dual-release-index.json",
        json.dumps(
            {
                "schema_version": 2,
                "git_commit": "commit",
                "git_tree": "tree",
                "bundles": bundles,
            }
        ),
    )


class ForwardOnlyAgentInstallerTest(unittest.TestCase):
    def test_failed_activation_keeps_new_release_and_records_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            home = root / "home"
            install_root = root / "install"
            release = install_root / "releases/new"
            old = install_root / "releases/old/deepseek"
            old.mkdir(parents=True)
            make_dual_release(release)
            install_root.mkdir(parents=True, exist_ok=True)
            (install_root / "current").symlink_to(old)
            env_file = root / "worker.env"
            write(env_file, "KRW_AGENT_DATABASE_URL=postgres://example\n", 0o600)
            bin_dir = install_command_stubs(root)
            launch_log = root / "launchctl.log"
            environment = {
                **os.environ,
                "HOME": str(home),
                "PATH": f"{bin_dir}:{os.environ['PATH']}",
                "KRW_AGENT_LOCAL_INSTALL_ROOT": str(install_root),
                "KRW_TEST_LAUNCHCTL_LOG": str(launch_log),
            }
            result = subprocess.run(
                [
                    "/bin/bash",
                    str(AGENT_INSTALLER),
                    "--mode",
                    "activate",
                    "--release-id",
                    "new",
                    "--provider",
                    "deepseek",
                    "--env-file",
                    str(env_file),
                ],
                text=True,
                capture_output=True,
                env=environment,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(
                (install_root / "current").resolve(), (release / "deepseek").resolve()
            )
            status = json.loads(
                (install_root / "deploy-state/new/activation-status.json").read_text(
                    encoding="utf-8"
                )
            )
            self.assertEqual(status["status"], "failed")
            self.assertEqual(status["component"], "krw-agentd")
            self.assertNotIn("rollback", result.stderr.lower())

    def test_rollback_mode_is_not_an_installer_operation(self) -> None:
        result = subprocess.run(
            [
                "/bin/bash",
                str(AGENT_INSTALLER),
                "--mode",
                "rollback",
                "--release-id",
                "anything",
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)


class ForwardOnlyCapabilityInstallerTest(unittest.TestCase):
    def test_failed_activation_does_not_restore_gateway_and_records_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary).resolve()
            home = root / "home"
            install_root = root / "install"
            operator = root / "operator"
            bundle = install_root / "releases/new/deepseek"
            bundle.mkdir(parents=True)
            install_root.mkdir(parents=True, exist_ok=True)
            (install_root / "current").symlink_to(bundle)
            identity = {
                "ok": True,
                "build_id": "build-new",
                "tool_schema_sha256": EMPTY_SHA256,
                "release_manifest_sha256": EMPTY_SHA256,
                "protocol_version": "2025-06-18",
                "tool_count": 1,
            }
            write(bundle / "capability-runtime/identity.json", json.dumps(identity))
            runtime_source = root / "runtime-source"
            write(runtime_source / ".venv/pyvenv.cfg", "home = /old/python\n")
            write(runtime_source / ".venv/bin/python", "#!/bin/sh\nexit 0\n", 0o755)
            (runtime_source / ".python/bin").mkdir(parents=True)
            archive = bundle / "capability-runtime/capability-runtime.venv.tar.gz"
            archive.parent.mkdir(parents=True, exist_ok=True)
            with tarfile.open(archive, "w:gz") as handle:
                handle.add(runtime_source / ".venv", arcname=".venv")
                handle.add(runtime_source / ".python", arcname=".python")
            write(bundle / "packaging/launchd/krw-capabilityd-start-local", "#!/bin/sh\n", 0o755)
            write(
                bundle / "deployments/endpoint-registry.yaml",
                "endpoint_registry:\n  - endpoint_ref: krw-ontology-local\n    origin: https://127.0.0.1:21432\n",
            )
            write(
                bundle / "deployments/deployment-binding.yaml",
                "deployment_bindings:\n"
                "  - binding_key: ontology\n"
                "    endpoint_ref: krw-ontology-local\n"
                "    tool_session_reuse: run-scoped\n"
                f"    server_build: {identity['build_id']}\n"
                f"    server_schema_bundle_hash: {EMPTY_SHA256}\n"
                f"    data_release_hash: {EMPTY_SHA256}\n",
            )
            write(operator / "runtime/capabilityd.env", "KRW_CAPABILITYD_PORT=19432\n")
            old_gateway = {"listen": {}, "tls": {}, "marker": "old"}
            for name in ("ontology", "feed", "filings", "guru"):
                write(operator / f"config/runtime/{name}-tls.json", json.dumps(old_gateway))
            bin_dir = install_command_stubs(root)
            launch_log = root / "launchctl.log"
            environment = {
                **os.environ,
                "HOME": str(home),
                "PATH": f"{bin_dir}:{os.environ['PATH']}",
                "KRW_AGENT_LOCAL_INSTALL_ROOT": str(install_root),
                "KRW_TEST_LAUNCHCTL_LOG": str(launch_log),
            }
            result = subprocess.run(
                [
                    "/bin/bash",
                    str(CAPABILITY_INSTALLER),
                    "--mode",
                    "activate",
                    "--release-id",
                    "new",
                    "--provider",
                    "deepseek",
                    "--operator-root",
                    str(operator),
                ],
                text=True,
                capture_output=True,
                env=environment,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            gateway = json.loads(
                (operator / "config/runtime/ontology-tls.json").read_text(encoding="utf-8")
            )
            self.assertEqual(
                gateway.get("service"),
                "krw-capabilityd",
                msg=f"stdout={result.stdout}\nstderr={result.stderr}",
            )
            status = json.loads(
                (
                    install_root
                    / "deploy-state/capabilityd-new/activation-status.json"
                ).read_text(encoding="utf-8")
            )
            self.assertEqual(status["status"], "failed")
            self.assertEqual(status["component"], "krw-capabilityd")

    def test_rollback_mode_is_not_an_installer_operation(self) -> None:
        result = subprocess.run(
            [
                "/bin/bash",
                str(CAPABILITY_INSTALLER),
                "--mode",
                "rollback",
                "--release-id",
                "anything",
                "--provider",
                "deepseek",
                "--operator-root",
                "/missing",
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
