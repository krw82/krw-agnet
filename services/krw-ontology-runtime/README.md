# KRW capability runtime

This is the canonical online, read-only capability runtime for `krw-agnet`.
It is deliberately independent from the `krw-ontology` checkout at runtime.

The initial domain import is locked by `UPSTREAM_LOCK.json` and must be
performed with `scripts/import_capability_runtime.sh`. The script reads only
the immutable Git object named in the lock; it never copies the source
working tree, data releases, caches, credentials, plugins, or build pipeline.

After import, this package owns the online implementation. Its MCP transport
is rebuilt around typed descriptors and direct-root contracts; upstream
FastMCP registration code is intentionally not imported.
