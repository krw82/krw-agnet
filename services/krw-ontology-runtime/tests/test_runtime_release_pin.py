from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from krw_capability_runtime.mcp_server import runtime, tools


def _write_release(root: Path, release_id: str) -> dict[str, object]:
    root.mkdir(parents=True)
    manifest: dict[str, object] = {
        "format": "krw-ontology-release/v3",
        "release_id": release_id,
        "global_spine_path": "indexes/global_spine.sqlite",
    }
    (root / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    return manifest


def _verified_release(root: Path, manifest: dict[str, object]) -> dict[str, object]:
    return {
        "ok": True,
        "errors": [],
        "root": str(root),
        "manifest_path": str(root / "manifest.json"),
        "manifest": manifest,
        "env": "prod",
        "release_id": manifest["release_id"],
        "index_layout": "global-spine",
    }


def test_runtime_resolves_current_once_and_keeps_the_physical_release(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    env_root = tmp_path / "prod"
    release_a = env_root / "release-a"
    release_b = env_root / "release-b"
    manifest_a = _write_release(release_a, "release-a")
    _write_release(release_b, "release-b")
    current = env_root / "current"
    current.symlink_to("release-a")

    def fake_verify(
        root: Path,
        *,
        env: str | None,
        manifest_path: Path,
        require_current_symlink: bool,
        **_kwargs: object,
    ) -> dict[str, object]:
        assert root == release_a.resolve()
        assert manifest_path == release_a.resolve() / "manifest.json"
        assert env == "prod"
        assert require_current_symlink is False
        return _verified_release(release_a.resolve(), manifest_a)

    monkeypatch.setattr(runtime, "verify_release_startup_v3", fake_verify)
    monkeypatch.delenv("KRW_ONTOLOGY_MANIFEST_PATH", raising=False)
    verification = runtime.prepare_mcp_runtime(
        root=current,
        env="prod",
        expected_release_id="release-a",
    )

    current.unlink()
    current.symlink_to("release-b")

    physical_root = release_a.resolve()
    assert verification["admission_pointer"] == str(current.absolute())
    assert verification["admitted_release_root"] == str(physical_root)
    assert verification["runtime_root"] == str(physical_root)
    assert Path(os.environ["KRW_ONTOLOGY_RELEASE_ROOT"]) == physical_root
    assert Path(os.environ["KRW_ONTOLOGY_GLOBAL_SPINE_PATH"]) == (
        physical_root / "indexes/global_spine.sqlite"
    )


def test_runtime_rejects_a_pointer_rotation_during_admission(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    env_root = tmp_path / "prod"
    release_a = env_root / "release-a"
    release_b = env_root / "release-b"
    manifest_a = _write_release(release_a, "release-a")
    _write_release(release_b, "release-b")
    current = env_root / "current"
    current.symlink_to("release-a")

    def rotate_after_verification(
        root: Path,
        **_kwargs: object,
    ) -> dict[str, object]:
        assert root == release_a.resolve()
        current.unlink()
        current.symlink_to("release-b")
        return _verified_release(release_a.resolve(), manifest_a)

    monkeypatch.setattr(runtime, "verify_release_startup_v3", rotate_after_verification)
    with pytest.raises(RuntimeError, match="pointer changed"):
        runtime.prepare_mcp_runtime(root=current, env="prod")


def test_persistent_store_pool_fails_closed_if_an_immutable_index_changes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    old_signature = tools._IndexSignature(
        "/releases/a/indexes/global_spine.sqlite",
        1,
        10,
        "release-a",
        "sha256:" + "a" * 64,
        "sha256:" + "b" * 64,
        "sha256:" + "c" * 64,
    )
    changed_signature = tools._IndexSignature(
        "/releases/b/indexes/global_spine.sqlite",
        2,
        11,
        "release-b",
        "sha256:" + "d" * 64,
        "sha256:" + "e" * 64,
        "sha256:" + "f" * 64,
    )
    signatures = iter((old_signature, changed_signature))

    class Store:
        def close(self) -> None:
            return None

    pool = tools._PersistentStorePool()
    monkeypatch.setattr(tools, "_index_signature", lambda _path: next(signatures))
    monkeypatch.setattr(pool, "_verify_signature", lambda _signature: None)
    monkeypatch.setattr(tools, "open_ontology_store", lambda *_args, **_kwargs: Store())

    with pool.acquire(Path("/immutable/indexes/global_spine.sqlite")):
        pass
    with pytest.raises(RuntimeError, match="immutable_release_identity_changed"):
        pool.acquire(Path("/immutable/indexes/global_spine.sqlite"))

    status = pool.status()
    assert status["identity_changed"] is True
    assert status["identity_change"]["previous_release_id"] == "release-a"
    assert status["identity_change"]["observed_release_id"] == "release-b"
