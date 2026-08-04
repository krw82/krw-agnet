import { constants as fsConstants } from "node:fs";
import { open, realpath } from "node:fs/promises";
import { isAbsolute, normalize, parse, resolve } from "node:path";
import { createHash } from "node:crypto";

import type { PublicReleaseDescriptor } from "./contracts.js";
import {
  canonicalHash,
  canonicalJson,
  isContentHash,
  type ContentHash,
  type JsonValue,
} from "./json.js";
import {
  ContractViolation,
  isJsonValue,
  validateReleaseDescriptor,
} from "./validation.js";

const DEFAULT_MAX_DESCRIPTOR_BYTES = 2 * 1024 * 1024;
const FORBIDDEN_KEY_PARTS = ["secret", "credential", "endpoint", "prompt"];
const FORBIDDEN_EXACT_KEYS = new Set([
  "api_base",
  "api_key",
  "headers",
  "url",
]);
const SENSITIVE_VALUE_PATTERNS = [
  /(?:https?|wss?):\/\//i,
  /\bBearer\s+[A-Za-z0-9._~+/=-]+/i,
  /\b(?:api[_-]?key|access[_-]?token|client[_-]?secret)\s*[:=]/i,
  /\b(?:sk|ds|rk)-[A-Za-z0-9_-]{12,}\b/,
];

export interface PinnedReleaseArtifact {
  readonly descriptor: PublicReleaseDescriptor;
  readonly artifact_hash: ContentHash;
  readonly release_set_hash: ContentHash;
  readonly file_identity: {
    readonly device: bigint;
    readonly inode: bigint;
    readonly size_bytes: number;
  };
}

export interface LoadReleaseArtifactOptions {
  readonly path: string;
  readonly expectedArtifactHash: ContentHash;
  readonly expectedReleaseSetHash: ContentHash;
  readonly maxBytes?: number;
}

/**
 * Pins one daemon-generated descriptor for the lifetime of the host process.
 * Deployments replace the artifact and restart the host; runs never reread it.
 */
export async function loadPinnedReleaseArtifact(
  options: LoadReleaseArtifactOptions,
): Promise<PinnedReleaseArtifact> {
  const artifactPath = validateArtifactPath(options.path);
  if (!isContentHash(options.expectedArtifactHash)) {
    throw new ContractViolation("invalid_expected_descriptor_hash");
  }
  if (!isContentHash(options.expectedReleaseSetHash)) {
    throw new ContractViolation("invalid_expected_release_set_hash");
  }
  const maxBytes = options.maxBytes ?? DEFAULT_MAX_DESCRIPTOR_BYTES;
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1 || maxBytes > 8 * 1024 * 1024) {
    throw new ContractViolation("invalid_descriptor_size_limit");
  }

  const parent = resolve(artifactPath, "..");
  let canonicalParent: string;
  try {
    canonicalParent = await realpath(parent);
  } catch {
    throw new ContractViolation("descriptor_parent_unavailable");
  }
  if (canonicalParent === parse(canonicalParent).root) {
    throw new ContractViolation("unsafe_descriptor_parent");
  }
  const canonicalPath = resolve(canonicalParent, parse(artifactPath).base);

  let handle;
  try {
    handle = await open(
      canonicalPath,
      fsConstants.O_RDONLY | (fsConstants.O_NOFOLLOW ?? 0),
    );
  } catch {
    throw new ContractViolation("descriptor_open_failed");
  }
  try {
    const before = await handle.stat({ bigint: true });
    if (!before.isFile()) throw new ContractViolation("descriptor_not_regular_file");
    if ((before.mode & 0o022n) !== 0n) {
      throw new ContractViolation("descriptor_writable_by_others");
    }
    if (before.size < 1n || before.size > BigInt(maxBytes)) {
      throw new ContractViolation("descriptor_size_out_of_bounds");
    }

    const bytes = Buffer.alloc(Number(before.size));
    let offset = 0;
    while (offset < bytes.length) {
      const chunk = await handle.read(bytes, offset, bytes.length - offset, offset);
      if (chunk.bytesRead === 0) throw new ContractViolation("descriptor_short_read");
      offset += chunk.bytesRead;
    }
    const after = await handle.stat({ bigint: true });
    if (
      after.dev !== before.dev ||
      after.ino !== before.ino ||
      after.size !== before.size ||
      after.mtimeNs !== before.mtimeNs
    ) {
      throw new ContractViolation("descriptor_changed_during_read");
    }

    const artifactHash = contentHashBytes(bytes);
    if (artifactHash !== options.expectedArtifactHash) {
      throw new ContractViolation("descriptor_artifact_hash_mismatch");
    }
    let text: string;
    try {
      text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    } catch {
      throw new ContractViolation("descriptor_invalid_utf8");
    }
    let value: unknown;
    try {
      value = JSON.parse(text);
    } catch {
      throw new ContractViolation("descriptor_invalid_json");
    }
    if (!isJsonValue(value)) throw new ContractViolation("descriptor_not_json_domain");
    if (canonicalJson(value) !== text) {
      throw new ContractViolation("descriptor_not_canonical_jcs");
    }
    rejectSensitiveMaterial(value);
    validateReleaseDescriptor(value as PublicReleaseDescriptor);
    const descriptor = value as PublicReleaseDescriptor;
    if (descriptor.release_set_hash !== options.expectedReleaseSetHash) {
      throw new ContractViolation("descriptor_release_set_hash_mismatch");
    }

    return Object.freeze({
      descriptor: deepFreeze(descriptor),
      artifact_hash: artifactHash,
      release_set_hash: options.expectedReleaseSetHash,
      file_identity: Object.freeze({
        device: before.dev,
        inode: before.ino,
        size_bytes: Number(before.size),
      }),
    });
  } finally {
    await handle.close();
  }
}

export function validatePinnedReleaseArtifact(value: PinnedReleaseArtifact): void {
  if (!isPlainObject(value)) throw new ContractViolation("release_artifact_object");
  const actual = Object.keys(value).sort();
  const expected = ["descriptor", "artifact_hash", "release_set_hash", "file_identity"].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new ContractViolation("release_artifact_shape");
  }
  if (!isContentHash(value.artifact_hash) || !isContentHash(value.release_set_hash)) {
    throw new ContractViolation("release_artifact_hash");
  }
  validateReleaseDescriptor(value.descriptor);
  if (
    canonicalHash(value.descriptor) !== value.artifact_hash ||
    value.descriptor.release_set_hash !== value.release_set_hash
  ) {
    throw new ContractViolation("release_artifact_pin_mismatch");
  }
  if (!isPlainObject(value.file_identity)) {
    throw new ContractViolation("release_artifact_file_identity");
  }
  const identityKeys = Object.keys(value.file_identity).sort();
  const expectedIdentityKeys = ["device", "inode", "size_bytes"].sort();
  if (
    identityKeys.length !== expectedIdentityKeys.length ||
    identityKeys.some((key, index) => key !== expectedIdentityKeys[index]) ||
    typeof value.file_identity.device !== "bigint" ||
    value.file_identity.device < 0n ||
    typeof value.file_identity.inode !== "bigint" ||
    value.file_identity.inode < 0n ||
    !Number.isSafeInteger(value.file_identity.size_bytes) ||
    value.file_identity.size_bytes < 1 ||
    value.file_identity.size_bytes !== Buffer.byteLength(canonicalJson(value.descriptor), "utf8") ||
    value.file_identity.size_bytes > 8 * 1024 * 1024
  ) {
    throw new ContractViolation("release_artifact_file_identity");
  }
}

function validateArtifactPath(value: string): string {
  if (
    value.length === 0 ||
    value.includes("\0") ||
    !isAbsolute(value) ||
    normalize(value) !== value ||
    value === parse(value).root
  ) {
    throw new ContractViolation("unsafe_descriptor_path");
  }
  return value;
}

function rejectSensitiveMaterial(value: JsonValue, depth = 0): void {
  if (depth > 64) throw new ContractViolation("descriptor_too_deep");
  if (typeof value === "string") {
    if (value.includes("\0") || SENSITIVE_VALUE_PATTERNS.some((pattern) => pattern.test(value))) {
      throw new ContractViolation("descriptor_contains_sensitive_value");
    }
    return;
  }
  if (value === null || typeof value !== "object") return;
  if (Array.isArray(value)) {
    for (const nested of value) rejectSensitiveMaterial(nested, depth + 1);
    return;
  }
  for (const [key, nested] of Object.entries(value)) {
    const normalized = key.toLowerCase();
    if (
      FORBIDDEN_EXACT_KEYS.has(normalized) ||
      FORBIDDEN_KEY_PARTS.some((part) => normalized.includes(part))
    ) {
      throw new ContractViolation("descriptor_contains_forbidden_field");
    }
    rejectSensitiveMaterial(nested, depth + 1);
  }
}

function contentHashBytes(bytes: Uint8Array): ContentHash {
  const digest = createHash("sha256").update(bytes).digest("hex");
  return `sha256:${digest}`;
}

function deepFreeze<T>(value: T): T {
  if (value !== null && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const nested of Object.values(value)) deepFreeze(nested);
  }
  return value;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}
