import { createHash } from "node:crypto";

export type JsonPrimitive = null | boolean | number | string;
export type JsonObject = { readonly [key: string]: JsonValue };
export type JsonValue = JsonPrimitive | JsonObject | readonly JsonValue[];

export type ContentHash = `sha256:${string}`;

const CONTENT_HASH_PATTERN = /^sha256:[0-9a-f]{64}$/;

export function isContentHash(value: unknown): value is ContentHash {
  return typeof value === "string" && CONTENT_HASH_PATTERN.test(value);
}

/**
 * RFC 8785 JSON Canonicalization Scheme for JSON-domain values.
 *
 * JavaScript's string ordering and JSON number/string serialization are the
 * primitives required by JCS. Non-JSON values, sparse arrays, cycles, custom
 * prototypes and non-finite numbers are rejected instead of being silently
 * rewritten as JSON.stringify would do.
 */
export function canonicalJson(value: JsonValue): string {
  return canonicalize(value, new Set<object>());
}

export function canonicalHash(value: JsonValue): ContentHash {
  const digest = createHash("sha256").update(canonicalJson(value), "utf8").digest("hex");
  return `sha256:${digest}`;
}

/** SHA-256 of exact UTF-8 bytes (not JCS-quoted text). */
export function utf8ContentHash(value: string): ContentHash {
  assertValidUnicode(value);
  return bytesContentHash(Buffer.from(value, "utf8"));
}

export function bytesContentHash(value: Uint8Array): ContentHash {
  const digest = createHash("sha256").update(value).digest("hex");
  return `sha256:${digest}`;
}

function canonicalize(value: JsonValue, ancestors: Set<object>): string {
  if (value === null) return "null";
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "string") {
    assertValidUnicode(value);
    return JSON.stringify(value);
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new TypeError("non-finite JSON number");
    return Object.is(value, -0) ? "0" : JSON.stringify(value);
  }

  if (ancestors.has(value)) throw new TypeError("cyclic JSON value");
  ancestors.add(value);
  try {
    if (Array.isArray(value)) {
      for (let index = 0; index < value.length; index += 1) {
        if (!Object.hasOwn(value, index)) throw new TypeError("sparse JSON array");
      }
      return `[${value.map((item) => canonicalize(item, ancestors)).join(",")}]`;
    }

    const object = value as JsonObject;
    const prototype = Object.getPrototypeOf(object);
    if (prototype !== Object.prototype && prototype !== null) {
      throw new TypeError("JSON object has a custom prototype");
    }
    const keys = Object.keys(object).sort();
    const members = keys.map((key) => {
      assertValidUnicode(key);
      const member = object[key];
      if (member === undefined) throw new TypeError("undefined JSON member");
      return `${JSON.stringify(key)}:${canonicalize(member, ancestors)}`;
    });
    return `{${members.join(",")}}`;
  } finally {
    ancestors.delete(value);
  }
}

function assertValidUnicode(value: string): void {
  for (let index = 0; index < value.length; index += 1) {
    const unit = value.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) {
        throw new TypeError("unpaired UTF-16 high surrogate");
      }
      index += 1;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) {
      throw new TypeError("unpaired UTF-16 low surrogate");
    }
  }
}
