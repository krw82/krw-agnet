# Production deployment templates

These are **PRODUCTION TEMPLATES** with placeholder identity values. They
mirror the structure of `deployments/local/` but every fixture-only identity
field (zero hash, `fixture` build) has been replaced by a clearly marked
`REPLACE_WITH_*` placeholder that passes Production-mode validation. They are
not runnable as-is: an operator must substitute real values before startup
admission.

## What is a placeholder

Any value prefixed `REPLACE_WITH_` MUST be replaced before use. The daemon's
Production validation (`crates/runtime-config/src/lib.rs`,
`validate_binding`) rejects:

- `server_schema_bundle_hash: sha256:0000…0000` (zero hash)
- `data_release_hash: sha256:0000…0000` (zero hash)
- `server_build: fixture`

so these templates ship `sha256:REPLACE_WITH_*` and
`REPLACE_WITH_BUILD_ID` instead, which are non-zero and non-`fixture` and
therefore structurally admissible. They are not real fingerprints.

## Producing the real identity values

After the immutable ontology release has been admitted, an operator obtains
the three real, non-secret identity values without starting the listener:

```bash
cd ~/krw-agnet/services/krw-ontology-runtime
uv run krw-capabilityd --print-identity
```

This emits:

- `build_id`            -> substitute for `REPLACE_WITH_BUILD_ID`
- `tool_schema_sha256`  -> substitute for `REPLACE_WITH_TOOL_SCHEMA_SHA256`
- `release_manifest_sha256` -> substitute for `REPLACE_WITH_RELEASE_MANIFEST_SHA256`

Copy those three values into `deployment-binding.example.yaml`,
`deployment-binding.krw-ontology.example.yaml`, and the three matching
`KRW_CAPABILITYD_EXPECTED_*` environment variables.

## Producing the real transport values

- In `endpoint-registry.example.yaml`, substitute the real public host for
  `REPLACE_WITH_PROD_ORIGIN` (it must start with `https://`).
- Set `tls_ca_pem_env` to the real name of the environment variable that
  holds the PEM-encoded pinned CA bundle for each endpoint. The name must be
  uppercase ASCII (`[A-Z][A-Z0-9_]*`).

## Verifying before startup

Verify the assembled set with the daemon's Production-mode check before
starting the listener:

```bash
uv run krw-agentd --check
```

`--check` runs the same `ValidationMode::Production` admission pass that the
listener runs at startup; it exits non-zero if any placeholder survives or any
identity pin is mismatched.

## Differences from `deployments/local/`

These templates intentionally diverge from `deployments/local/` in the
following ways:

- **Real TLS profile.** Every endpoint uses `system-plus-pinned-ca-v1` with a
  required `tls_ca_pem_env`, instead of the local `system-roots-v1`.
- **Non-loopback origins.** Origins are real public hosts
  (`https://REPLACE_WITH_PROD_ORIGIN`), not the local `https://krw-agent.local`.
- **Real identity pins.** Every capability binding carries non-zero, non-fixture
  placeholders for `server_schema_bundle_hash`, `server_build`, and
  `data_release_hash`. The local catalog ships the zero hash / `fixture`
  fingerprint on purpose, which Production admission rejects.
- **No fixture builds.** `server_build` is never `fixture`.
- **Production registry ids.** `deployment_id`, `registry_id`, and endpoint
  `registry_id` carry production-appropriate ids (`production-contract`,
  `krw-ontology-production-v1`, `production-runtime`) instead of the
  `local-*` ids.

See `deployments/local/README.md` for the local-only setup that admits
fixture fingerprints and loopback origins.
