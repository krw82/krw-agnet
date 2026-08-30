# KRW capability runtime

This is the canonical online, read-only capability runtime for `krw-agnet`.
It is deliberately independent from the `krw-ontology` checkout at runtime.

The domain import is locked by `UPSTREAM_LOCK.json` and is performed with
`scripts/import_capability_runtime.sh`. The script reads only the immutable
Git object named in the lock; it never copies the source working tree, data
releases, caches, credentials, plugins, or build pipeline.

After import, this package owns the online implementation. Its MCP transport
is rebuilt around typed descriptors and direct-root contracts; upstream
FastMCP registration code is intentionally not imported.

## Mixed provenance: upstream-owned vs runtime-native files

Every file under `src/krw_capability_runtime` is exactly one of:

- **Upstream-owned** — listed in the script's `SOURCE_PATHS` (recorded as
  `module_paths` in `UPSTREAM_LOCK.json`). Its content is the pinned
  `krw-ontology` commit modulo the mechanical `krw_ontology →
  krw_capability_runtime` package rename, plus reviewed canonical runtime
  edits made on top (for example the packaged-resource path candidates in
  `validators/metric_validator.py` and `observation/seed.py`, and the serving
  copy of `observation/__init__.py` without the build-time builder imports).
- **Runtime-native** — everything else (`market/`, `transport/`,
  `mcp_server/runtime.py` glue where locally evolved, `observation/tools.py`,
  `__main__.py`, …). These files are ours; the import script never writes
  them.

The lock stores, per upstream-owned file, both the `upstream_sha256` (the
pinned commit, rename applied) and the `runtime_sha256` (the bytes actually
in this tree when the lock was recorded). Files where the two differ are
flagged `diverged: true`: that flag is a *record of reality* — canonical
edits and files whose content still predates the pin — not an error. The lock
records what is, never what we aspire to import.

## Import script modes

Run from anywhere; the optional argument is the source repository (defaults
to the v2 `krw-ontology` checkout):

- `import_capability_runtime.sh [REPO]` — **initial** wholesale import into a
  clean tree. Refuses to overwrite an existing `src/krw_capability_runtime`,
  applies the package rename, lands the `ontology/` resources under
  `src/krw_capability_runtime/resources/ontology/`, and writes the lock.
- `import_capability_runtime.sh --update [REPO]` — **overlay update**:
  refreshes every `SOURCE_PATHS` file from the pinned commit in place.
  Refuses to run when the runtime tree (or the script itself) is dirty, so
  every content change is attributable to one reviewed step. Never touches
  runtime-native files. After it runs, re-apply reviewed canonical edits on
  top, then run `--record` and commit.
- `import_capability_runtime.sh --record [REPO]` — rewrite
  `UPSTREAM_LOCK.json` from the **current** tree without changing any source
  file. Use after intentional canonical edits, after `--update`, or after
  removing an upstream-deleted path from `SOURCE_PATHS`.
- `import_capability_runtime.sh --verify [REPO]` — recompute every per-file
  hash and diff against the lock. Exits nonzero when the tree no longer
  matches the recorded lock (post-lock edits), when the lock is missing or
  stale relative to `SOURCE_PATHS`, or when the lock's pin differs from the
  script's pin. Also prints the files recorded as intentionally diverged
  from the pin. Run this in CI or before sealing a release.

Upstream paths deleted at the pin (for example `ontology/schema/objects.yaml`)
must be dropped from `SOURCE_PATHS` and removed from the packaged resources;
`--record` then snapshots the new inventory.
