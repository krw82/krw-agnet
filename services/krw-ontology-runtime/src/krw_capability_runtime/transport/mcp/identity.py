"""Verified identity for the canonical HTTP MCP sidecar.

The sidecar does not accept a caller-provided schema or release fingerprint.
It derives its identity from the registry that actually serves ``tools/list``
and from the release that startup verification already admitted.  A deployment
then pins these three values independently in its ``DeploymentBinding``:

* executable/runtime build identity;
* complete exposed MCP tool-schema bundle;
* immutable ontology release manifest bytes.

This is intentionally a small, closed ABI.  It contains no endpoint, secret,
or principal information and it does not enable stateless MCP-session reuse.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
from collections.abc import Mapping
from dataclasses import dataclass
from importlib.metadata import PackageNotFoundError, version
from pathlib import Path
from typing import Any

from krw_capability_runtime.transport.mcp.descriptors import CapabilityRegistry

SERVICE_NAME = "krw-capabilityd"
READINESS_SCHEMA_VERSION = "krw-capabilityd/readiness/v1"
TOOL_SCHEMA_BUNDLE_VERSION = "krw-capabilityd/tool-schema-bundle/v1"
BUILD_ID_INPUT_VERSION = "krw-capabilityd/build-id-input/v1"
MCP_PROTOCOL_VERSION = "2025-06-18"

_HASH_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
_BUILD_ID_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,255}\Z")
_MAX_SOURCE_FILES = 512
_MAX_SOURCE_BYTES = 16 * 1024 * 1024
_MAX_TOOL_COUNT = 512
_EXPECTED_BUILD_ID_ENV = "KRW_CAPABILITYD_EXPECTED_BUILD_ID"
_EXPECTED_TOOL_SCHEMA_ENV = "KRW_CAPABILITYD_EXPECTED_TOOL_SCHEMA_SHA256"
_EXPECTED_RELEASE_ENV = "KRW_CAPABILITYD_EXPECTED_RELEASE_MANIFEST_SHA256"


class ServiceIdentityError(RuntimeError):
    """The sidecar cannot prove the identity that its deployment must pin."""


@dataclass(frozen=True)
class CapabilityServiceIdentity:
    """Immutable, non-secret identity emitted by the HTTPS readiness route."""

    build_id: str
    tool_schema_sha256: str
    release_manifest_sha256: str
    protocol_version: str = MCP_PROTOCOL_VERSION

    def __post_init__(self) -> None:
        if not _BUILD_ID_RE.fullmatch(self.build_id):
            raise ServiceIdentityError("build_id must be a bounded opaque build identifier")
        for field, value in (
            ("tool_schema_sha256", self.tool_schema_sha256),
            ("release_manifest_sha256", self.release_manifest_sha256),
        ):
            if not _HASH_RE.fullmatch(value):
                raise ServiceIdentityError(f"{field} must be a lowercase sha256: digest")
        if self.protocol_version != MCP_PROTOCOL_VERSION:
            raise ServiceIdentityError(
                f"protocol_version must be the closed MCP ABI {MCP_PROTOCOL_VERSION}"
            )

    def readiness_document(self, *, tool_count: int) -> dict[str, Any]:
        """Return the bounded document Rust validates before MCP initialize.

        ``fingerprint_match`` means that this listener was constructed only
        after verified release admission and immutable identity construction.
        The client still compares all three fields against its independently
        resolved deployment pins; this flag never substitutes for that check.
        """

        if not 1 <= tool_count <= _MAX_TOOL_COUNT:
            raise ServiceIdentityError("tool_count is outside the closed readiness range")
        return {
            "schema_version": READINESS_SCHEMA_VERSION,
            "ok": True,
            "fingerprint_match": True,
            "service": SERVICE_NAME,
            "transport": "streamable-http",
            "protocol_version": self.protocol_version,
            "build_id": self.build_id,
            "tool_schema_sha256": self.tool_schema_sha256,
            "release_manifest_sha256": self.release_manifest_sha256,
            "tool_count": tool_count,
        }


def build_service_identity(
    registry: CapabilityRegistry,
    verification: Mapping[str, Any],
) -> CapabilityServiceIdentity:
    """Derive one listener identity from the admitted release and real tools.

    ``prepare_mcp_runtime`` is the only production producer of
    ``release_manifest_sha256``.  Requiring it here prevents a listener from
    inventing a data-release pin after startup validation.
    """

    if verification.get("ok") is not True:
        raise ServiceIdentityError("cannot derive sidecar identity from an unverified release")
    release_manifest_sha256 = verification.get("release_manifest_sha256")
    if not isinstance(release_manifest_sha256, str) or not _HASH_RE.fullmatch(
        release_manifest_sha256
    ):
        raise ServiceIdentityError(
            "verified runtime is missing release_manifest_sha256; use prepare_mcp_runtime"
        )
    return CapabilityServiceIdentity(
        build_id=_build_id(),
        tool_schema_sha256=_tool_schema_sha256(registry),
        release_manifest_sha256=release_manifest_sha256,
    )


def assert_expected_identity(
    identity: CapabilityServiceIdentity,
    *,
    require_all: bool,
    environ: Mapping[str, str] | None = None,
) -> None:
    """Fail startup when deployment-provided non-secret identity pins drift.

    Development may omit all three values while it discovers a newly built
    identity.  Production requires all of them, so a release cannot silently
    begin serving a different binary, tool surface, or ontology snapshot.
    """

    source = environ if environ is not None else os.environ
    expected = (
        (_EXPECTED_BUILD_ID_ENV, identity.build_id),
        (_EXPECTED_TOOL_SCHEMA_ENV, identity.tool_schema_sha256),
        (_EXPECTED_RELEASE_ENV, identity.release_manifest_sha256),
    )
    missing: list[str] = []
    for env_name, actual in expected:
        configured = source.get(env_name)
        if configured is None or not configured.strip():
            missing.append(env_name)
            continue
        if configured.strip() != actual:
            raise ServiceIdentityError(f"{env_name} does not match the admitted sidecar identity")
    if require_all and missing:
        raise ServiceIdentityError(
            "production sidecar startup requires identity pins: " + ", ".join(missing)
        )


def tool_schema_bundle(registry: CapabilityRegistry) -> dict[str, Any]:
    """Return the canonical complete physical MCP tool ABI bundle.

    The logical ID is included alongside the exact ``tools/list`` representation
    so a renamed physical MCP tool or a mapping change alters the fingerprint.
    It is derived from descriptors rather than from a manually duplicated list.
    """

    tools: list[dict[str, Any]] = []
    for descriptor in registry.descriptors():
        wire_tool = descriptor.as_mcp_tool().model_dump(
            mode="json", by_alias=True, exclude_none=True
        )
        tools.append(
            {
                "logical_capability_id": descriptor.logical_capability_id,
                "tool": wire_tool,
            }
        )
    return {
        "schema_version": TOOL_SCHEMA_BUNDLE_VERSION,
        "protocol_version": MCP_PROTOCOL_VERSION,
        "tools": tools,
    }


def _tool_schema_sha256(registry: CapabilityRegistry) -> str:
    return _sha256(_canonical_json_bytes(tool_schema_bundle(registry)))


def _build_id() -> str:
    source_hash = _package_source_sha256()
    payload = {
        "schema_version": BUILD_ID_INPUT_VERSION,
        "package_source_sha256": source_hash,
        "runtime": {
            "python": f"{sys.version_info.major}.{sys.version_info.minor}",
            "dependencies": {
                distribution: _distribution_version(distribution)
                for distribution in (
                    "krw-ontology-runtime",
                    "mcp",
                    "pydantic",
                    "starlette",
                    "uvicorn",
                )
            },
        },
    }
    return f"{SERVICE_NAME}-{_sha256(_canonical_json_bytes(payload))[7:31]}"


def _package_source_sha256() -> str:
    package_root = Path(__file__).resolve().parents[2]
    if not package_root.is_dir():
        raise ServiceIdentityError("capability runtime package source directory is unavailable")
    paths = sorted(package_root.rglob("*.py"), key=lambda path: path.relative_to(package_root).as_posix())
    if not paths or len(paths) > _MAX_SOURCE_FILES:
        raise ServiceIdentityError("capability runtime source inventory is outside the supported bound")
    total_bytes = 0
    records: list[dict[str, Any]] = []
    for path in paths:
        if path.is_symlink() or not path.is_file():
            raise ServiceIdentityError("capability runtime source inventory contains a non-regular file")
        data = path.read_bytes()
        total_bytes += len(data)
        if total_bytes > _MAX_SOURCE_BYTES:
            raise ServiceIdentityError("capability runtime source inventory exceeds the identity byte bound")
        records.append(
            {
                "path": path.relative_to(package_root).as_posix(),
                "sha256": _sha256(data),
                "size_bytes": len(data),
            }
        )
    return _sha256(
        _canonical_json_bytes(
            {
                "schema_version": "krw-capabilityd/source-bundle/v1",
                "files": records,
            }
        )
    )


def _distribution_version(distribution: str) -> str:
    try:
        return version(distribution)
    except PackageNotFoundError as error:
        raise ServiceIdentityError(
            f"required runtime distribution metadata is missing: {distribution}"
        ) from error


def _canonical_json_bytes(value: Any) -> bytes:
    """Serialize the versioned sidecar bundle deterministically.

    This fingerprint is deliberately defined by this exact bundle version and
    serializer, not conflated with AgentImage's RFC 8785 schema-artifact hash.
    A future serializer upgrade must change ``TOOL_SCHEMA_BUNDLE_VERSION`` and
    therefore produce a new deployment pin.
    """

    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise ServiceIdentityError("sidecar identity contains non-canonical JSON") from error


def _sha256(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()
