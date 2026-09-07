"""Closed provider/model mapping shared by standalone release tooling."""

from __future__ import annotations

import hashlib
import json
import pathlib

PROVIDER_MODELS: dict[str, str] = {
    "glm": "glm-5.3",
    "deepseek": "deepseek-v4-flash",
}

RELEASE_SCHEMA_VERSION = "krw-standalone-release/v2"


def validate_public_descriptor(
    path: pathlib.Path,
    provider: str,
    *,
    expected_artifact_hash: str | None = None,
    expected_release_set_hash: str | None = None,
) -> dict[str, object]:
    """Validate that a live runner is attached to the selected provider lane.

    The Gateway owns the descriptor and still performs its own pinned-load
    validation. This small read-only check prevents a quality runner from
    labelling a GLM Gateway report as DeepSeek (or vice versa) merely because
    the operator supplied a different command-line label.
    """

    if provider not in PROVIDER_MODELS:
        model_for_provider(provider)
    if not path.is_absolute() or path.is_symlink() or not path.is_file():
        raise ValueError("provider descriptor is missing or unsafe")
    raw = path.read_bytes()
    if not raw or len(raw) > 16 * 1024 * 1024:
        raise ValueError("provider descriptor is outside the allowed byte bound")
    observed_artifact_hash = "sha256:" + hashlib.sha256(raw).hexdigest()
    if expected_artifact_hash is not None and observed_artifact_hash != expected_artifact_hash:
        raise ValueError("provider descriptor artifact hash mismatch")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("provider descriptor is not valid JSON") from error
    if not isinstance(value, dict) or value.get("schema_version") != 3:
        raise ValueError("provider descriptor schema is invalid")
    release_set_hash = value.get("release_set_hash")
    if (
        not isinstance(release_set_hash, str)
        or len(release_set_hash) != 71
        or not release_set_hash.startswith("sha256:")
        or any(character not in "0123456789abcdef" for character in release_set_hash[7:])
    ):
        raise ValueError("provider descriptor release set hash is invalid")
    if expected_release_set_hash is not None and release_set_hash != expected_release_set_hash:
        raise ValueError("provider descriptor release set hash mismatch")
    entries = value.get("entries")
    expected_model = model_for_provider(provider)
    if not isinstance(entries, list) or not entries:
        raise ValueError("provider descriptor has no entry")
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("provider descriptor entry is invalid")
        execution = entry.get("execution")
        if not isinstance(execution, dict) or execution.get("resolved_model") != expected_model:
            raise ValueError("provider descriptor model does not match selected provider")
    return {
        "provider_id": provider,
        "physical_model": expected_model,
        "descriptor_artifact_hash": observed_artifact_hash,
        "release_set_hash": release_set_hash,
        "entry_count": len(entries),
    }


def model_for_provider(provider: str) -> str:
    """Return the only physical model allowed for a provider."""

    try:
        return PROVIDER_MODELS[provider]
    except KeyError as error:
        allowed = ", ".join(sorted(PROVIDER_MODELS))
        raise ValueError(
            f"unknown provider: {provider!r}; expected one of {allowed}"
        ) from error
