import assert from "node:assert/strict";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  ContractViolation,
  canonicalHash,
  canonicalJson,
  committedSourceFromCanonicalJcs,
  createProductProjectionHandlers,
  loadPinnedReleaseArtifact,
  parseAnswerCommitted,
  prepareEnqueueRun,
  sessionMemoryFrontierHash,
  utf8ContentHash,
  validatePreparedEnqueueRun,
  validateSessionMemoryCarrier,
  validateSessionMemoryViewClaim,
  type AnswerBillingProjectionV1,
  type AnswerCommittedProjectionV1,
  type AuthenticatedRunOwnershipV1,
  type BudgetLimits,
  type CommittedAnswerSourceClaimV1,
  type CommittedAnswerSourceMaterializationRequestV1,
  type ContentHash,
  type EnqueueRunIntentV1,
  type JsonValue,
  type OutboxReceipt,
  type PinnedReleaseArtifact,
  type ProductProjectionConsumers,
  type ProjectionApplyReceiptV1,
  type ProjectionAuthorityClaimV1,
  type ProjectionAuthorityRequestV1,
  type PublicReleaseDescriptor,
  type SessionMemoryCarrierV3,
  type SessionMemoryViewClaimV3,
} from "../src/index.js";

const hash = (digit: string) => `sha256:${digit.repeat(64)}` as ContentHash;

const ownership: AuthenticatedRunOwnershipV1 = {
  schema_version: 1,
  tenant_id: "tenant-01",
  principal_id: "principal-01",
  session_id: "123e4567-e89b-12d3-a456-426614174000",
  run_id: "123e4567-e89b-12d3-a456-426614174001",
};

const budget: BudgetLimits = {
  max_provider_turns: 4,
  max_capability_calls: 1,
  max_replans: 1,
  max_repairs: 1,
  max_input_tokens: 24_000,
  max_output_tokens: 6_000,
  max_evidence_bytes: 1_048_576,
  deadline_ms: 60_000,
  capability_call_limits: {},
};

function descriptor(contextKind: "existing_answer" | "question_only" = "existing_answer"):
  PublicReleaseDescriptor {
  return {
    schema_version: 3,
    release_set_hash: hash("0"),
    runtime_version: "krw-agentd-2026-08",
    entries: [
      {
        run_kind: contextKind === "existing_answer" ? "display_plan" : "router",
        locale: "ko-KR",
        agent_image_hash: hash("1"),
        model_profile: "glm_direct",
        scope: { context_kind: contextKind, cardinality: "exact", value: contextKind === "existing_answer" ? 1 : 0 },
        execution: {
          protocol_version: 7,
          agent_image_hash: hash("1"),
          deployment_binding_hash: hash("2"),
          model_registry_hash: hash("3"),
          budget_registry_hash: hash("4"),
          model_profile: "glm_direct",
          requested_model: "glm-5.2",
          resolved_model: "glm-5.2",
          provider_api_version: "anthropic-messages-v1",
          provider_max_context_tokens: 204_800,
          provider_wire_capabilities: {
            thinking: {
              supported: true,
              supports_tools: true,
              supports_tool_choice: true,
              supports_json_object: true,
              supports_json_schema_output: false,
              supports_strict_tool_input: false,
            },
            non_thinking: {
              supported: true,
              supports_tools: true,
              supports_tool_choice: true,
              supports_json_object: true,
              supports_json_schema_output: false,
              supports_strict_tool_input: false,
            },
            requires_thinking_block_replay: true,
            requires_assistant_content_for_tool_calls: false,
          },
          thinking: "disabled",
          reasoning_effort: null,
          capability_release_hashes: {},
          budget,
        },
      },
    ],
  };
}

function artifact(value = descriptor()): PinnedReleaseArtifact {
  return {
    descriptor: value,
    artifact_hash: canonicalHash(value),
    release_set_hash: value.release_set_hash,
    file_identity: {
      device: 1n,
      inode: 1n,
      size_bytes: Buffer.byteLength(canonicalJson(value), "utf8"),
    },
  };
}

function committedSource() {
  const canonical = {
    schema_version: 1,
    final_receipt_hash: hash("a"),
    answer_bundle_hash: hash("b"),
    answer_ir_hash: hash("c"),
    locale: "ko-KR",
    default_order: ["unit-01"],
    units: [
      {
        unit_id: "unit-01",
        unit_type: "paragraph",
        importance: "required",
        confidence: "direct",
        summary: "검증된 요약",
        content_hash: hash("d"),
        claim_ids: ["claim-01"],
        evidence_ids: ["evidence-01"],
      },
    ],
  } as const;
  return committedSourceFromCanonicalJcs(canonicalJson(canonical), {
    finalReceiptHash: canonical.final_receipt_hash,
    answerBundleHash: canonical.answer_bundle_hash,
    answerIrHash: canonical.answer_ir_hash,
  });
}

function memoryCarrier(revision = 7): SessionMemoryCarrierV3 {
  const semantic = {
    schema_version: 3,
    authority: "context_only",
    session_id_hash: utf8ContentHash(ownership.session_id),
    source_frontier_hash: hash("e"),
    source_revision: revision,
    tickers: [],
    constraints: [],
    sources: {},
    claims: [],
    unresolved_goals: [],
    recent_turns: [],
  } as const;
  const view = { ...semantic, view_hash: canonicalHash(semantic) };
  return {
    schema_version: 3,
    source_revision: revision,
    view_hash: canonicalHash(view),
    source_frontier_hash: semantic.source_frontier_hash,
    canonical_view: view,
  };
}

function existingAnswerIntent(): EnqueueRunIntentV1 {
  return {
    mutation_id: "enqueue-01",
    run_kind: "display_plan",
    locale: "ko-KR",
    question: "이 답변을 더 읽기 쉬운 구조로 바꿔줘.",
    context: {
      kind: "existing_answer",
      source_run_id: "123e4567-e89b-12d3-a456-426614174002",
    },
  };
}

function questionOnlyIntent(mutationId: string, question: string): EnqueueRunIntentV1 {
  return {
    mutation_id: mutationId,
    run_kind: "router",
    locale: "ko-KR",
    question,
    context: { kind: "question_only" },
  };
}

test("frontier-v2 transition is pinned to the Rust cross-language vector", () => {
  assert.equal(
    sessionMemoryFrontierHash(hash("1"), 7, hash("2")),
    "sha256:f2655508c5e2c43af0292f6e230fa57188befd420b2dc24c204d0b3a36744b42",
  );
});

test("safe enqueue materializes existing answer and leaves memory to the fenced worker", async () => {
  const source = committedSource();
  let committedRequest: CommittedAnswerSourceMaterializationRequestV1 | undefined;
  const prepared = await prepareEnqueueRun({
    artifact: artifact(),
    ownership,
    intent: existingAnswerIntent(),
    materializers: {
      committedAnswers: {
        async materializeCommittedAnswerSource(request) {
          committedRequest = request;
          const claim: CommittedAnswerSourceClaimV1 = {
            schema_version: 1,
            request_hash: canonicalHash(request),
            tenant_id: request.tenant_id,
            principal_id: request.principal_id,
            session_id: request.session_id,
            requesting_run_id: request.requesting_run_id,
            source_run_id: request.source_run_id,
            committed_source: source,
          };
          return claim;
        },
      },
    },
  });

  assert.equal(committedRequest?.tenant_id, ownership.tenant_id);
  assert.equal(committedRequest?.principal_id, ownership.principal_id);
  assert.equal(committedRequest?.session_id, ownership.session_id);
  assert.deepEqual(
    prepared.agent_request.immutable_snapshot.request.context,
    { kind: "existing_answer", committed_source: source },
  );
  assert.equal(prepared.agent_request.immutable_snapshot.request.session_memory, null);
  assert.equal(prepared.agent_request.immutable_snapshot.schema_version, 7);
  assert.equal(prepared.agent_request.immutable_snapshot.execution.protocol_version, 7);
  assert.equal(prepared.agent_request.immutable_snapshot.execution.requested_model, "glm-5.2");
  assert.equal(Object.isFrozen(prepared), true);
  assert.equal(Object.isFrozen(prepared.agent_request), true);
  validatePreparedEnqueueRun(prepared);
});

test("product chat bindings preserve member, room, and run identity across retries", async () => {
  const fixture = JSON.parse(
    await readFile(
      new URL("../../../fixtures/product-chat/v1/multi-user-chat-bindings.json", import.meta.url),
      "utf8",
    ),
  ) as {
    schema_version: number;
    suite_id: string;
    cases: Array<{
      case_id: string;
      tenant_id: string;
      principal_id: string;
      session_id: string;
      run_ids: string[];
    }>;
  };
  assert.equal(fixture.schema_version, 1);
  assert.equal(fixture.suite_id, "multi-user-chat-bindings-v1");
  assert.equal(fixture.cases.length, 3);

  for (const [caseIndex, binding] of fixture.cases.entries()) {
    assert.ok(binding.tenant_id.length > 0);
    assert.match(binding.principal_id, /^[0-9a-f-]{36}$/);
    assert.match(binding.session_id, /^[0-9a-f-]{36}$/);
    const firstRunId = binding.run_ids[0];
    assert.ok(firstRunId);
    const boundOwnership: AuthenticatedRunOwnershipV1 = {
      schema_version: 1,
      tenant_id: binding.tenant_id,
      principal_id: binding.principal_id,
      session_id: binding.session_id,
      run_id: firstRunId,
    };
    const intent = questionOnlyIntent(
      `binding-${caseIndex}-mutation`,
      `채팅방 ${binding.session_id}의 후속 리서치`,
    );
    const prepared = await prepareEnqueueRun({
      artifact: artifact(descriptor("question_only")),
      ownership: boundOwnership,
      intent,
      materializers: rejectingMaterializers(),
    });
    validatePreparedEnqueueRun(prepared);
    const request = prepared.agent_request;
    assert.equal(request.tenant_id, boundOwnership.tenant_id);
    assert.equal(request.principal_id, boundOwnership.principal_id);
    assert.equal(request.session_id, boundOwnership.session_id);
    assert.equal(request.run_id, boundOwnership.run_id);
    assert.equal(request.immutable_snapshot.request.tenant_id, boundOwnership.tenant_id);
    assert.equal(request.immutable_snapshot.request.principal_id, boundOwnership.principal_id);
    assert.equal(request.immutable_snapshot.request.session_id, boundOwnership.session_id);
    assert.equal(request.immutable_snapshot.request.run_id, boundOwnership.run_id);
    assert.equal(request.immutable_snapshot.request.session_memory, null);

    const retried = await prepareEnqueueRun({
      artifact: artifact(descriptor("question_only")),
      ownership: boundOwnership,
      intent,
      materializers: rejectingMaterializers(),
    });
    assert.equal(retried.preparation_hash, prepared.preparation_hash);
    assert.deepEqual(retried.agent_request, prepared.agent_request);
  }
});

test("client intent cannot inject a canonical answer or session-memory carrier", async () => {
  const injectedSource = {
    ...existingAnswerIntent(),
    context: {
      kind: "existing_answer",
      source_run_id: "123e4567-e89b-12d3-a456-426614174002",
      committed_source: committedSource(),
    },
  } as unknown as EnqueueRunIntentV1;
  await assert.rejects(
    () => prepareEnqueueRun({
      artifact: artifact(),
      ownership,
      intent: injectedSource,
      materializers: rejectingMaterializers(),
    }),
    /unknown_or_missing_materialization_field/,
  );

  const injectedMemory = {
    ...existingAnswerIntent(),
    session_memory: memoryCarrier(),
  } as unknown as EnqueueRunIntentV1;
  await assert.rejects(
    () => prepareEnqueueRun({
      artifact: artifact(),
      ownership,
      intent: injectedMemory,
      materializers: rejectingMaterializers(),
    }),
    /unknown_or_missing_materialization_field/,
  );
});

test("materializer claims are request-hash and ownership bound", async () => {
  const source = committedSource();
  await assert.rejects(
    () => prepareEnqueueRun({
      artifact: artifact(),
      ownership,
      intent: existingAnswerIntent(),
      materializers: {
        committedAnswers: {
          async materializeCommittedAnswerSource(request) {
            return {
              schema_version: 1,
              request_hash: canonicalHash(request),
              tenant_id: request.tenant_id,
              principal_id: "different-principal",
              session_id: request.session_id,
              requesting_run_id: request.requesting_run_id,
              source_run_id: request.source_run_id,
              committed_source: source,
            };
          },
        },
      },
    }),
    /committed_source_claim_ownership_mismatch/,
  );
});

test("session-memory audit claim is v3-only and binds worker resolution to ownership", () => {
  const carrier = memoryCarrier();
  assert.throws(
    () => validateSessionMemoryCarrier({ ...carrier, schema_version: 1 } as unknown as SessionMemoryCarrierV3),
    /session_memory_version/,
  );
  const claim: SessionMemoryViewClaimV3 = {
    schema_version: 3,
    run_id_hash: utf8ContentHash(ownership.run_id),
    session_id_hash: utf8ContentHash(ownership.session_id),
    source_revision: carrier.source_revision,
    source_frontier_hash: carrier.source_frontier_hash,
    view_hash: carrier.view_hash,
    page_count: 1,
    delta_count: carrier.source_revision,
    carrier,
  };
  validateSessionMemoryViewClaim(claim, ownership);
  assert.throws(
    () => validateSessionMemoryViewClaim(
      { ...claim, source_revision: carrier.source_revision + 1 },
      ownership,
    ),
    /session_memory_claim_carrier_mismatch/,
  );
});

test("release artifact loader pins canonical bytes, descriptor hash, and Flash-only contract", async () => {
  const directory = await mkdtemp(join(tmpdir(), "krw-host-release-"));
  try {
    const value = descriptor("question_only");
    const text = canonicalJson(value);
    const path = join(directory, "release.json");
    await writeFile(path, text, { encoding: "utf8", mode: 0o600 });
    await chmod(path, 0o600);
    const loaded = await loadPinnedReleaseArtifact({
      path,
      expectedArtifactHash: canonicalHash(value),
      expectedReleaseSetHash: value.release_set_hash,
    });
    assert.equal(loaded.descriptor.entries[0]?.execution.requested_model, "glm-5.2");
    assert.equal(Object.isFrozen(loaded.descriptor), true);

    await assert.rejects(
      () => loadPinnedReleaseArtifact({
        path,
        expectedArtifactHash: hash("f"),
        expectedReleaseSetHash: value.release_set_hash,
      }),
      /descriptor_artifact_hash_mismatch/,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("answer.committed is body-free and enforces all-null/all-non-null memory metadata", () => {
  const parsed = parseAnswerCommitted(outboxEvent("answer.committed", {
    run_id: ownership.run_id,
    answer_bundle_hash: hash("5"),
    rendered_message_hash: hash("6"),
    memory_revision: 8,
    memory_frontier_hash: hash("7"),
  }));
  assert.equal(parsed.memory_revision, 8);

  const withoutMemory = parseAnswerCommitted(outboxEvent("answer.committed", {
    run_id: ownership.run_id,
    answer_bundle_hash: hash("5"),
    rendered_message_hash: hash("6"),
    memory_revision: null,
    memory_frontier_hash: null,
  }));
  assert.equal(withoutMemory.memory_frontier_hash, null);

  assert.throws(
    () => parseAnswerCommitted(outboxEvent("answer.committed", {
      run_id: ownership.run_id,
      answer_bundle_hash: hash("5"),
      rendered_message_hash: hash("6"),
      memory_revision: 8,
      memory_frontier_hash: null,
    })),
    /answer_committed_memory_metadata_partial/,
  );
  assert.throws(
    () => parseAnswerCommitted(outboxEvent("answer.committed", {
      run_id: ownership.run_id,
      answer_bundle_hash: hash("5"),
      rendered_message_hash: hash("6"),
      memory_revision: null,
      memory_frontier_hash: null,
      canonical_source_jcs: "{}",
    })),
    /unknown_or_missing_projection_field/,
  );
});

test("projection consumers receive DB fence, stable idempotency key, safe SSE, and usage-only billing", async () => {
  const applied: Array<AnswerCommittedProjectionV1 | AnswerBillingProjectionV1> = [];
  const consumers = fixtureConsumers(applied);
  const handlers = createProductProjectionHandlers(fixtureAuthority(), consumers);
  const committed = outboxEvent("answer.committed", {
    run_id: ownership.run_id,
    answer_bundle_hash: hash("5"),
    rendered_message_hash: hash("6"),
    memory_revision: 8,
    memory_frontier_hash: hash("7"),
  });
  const billing = outboxEvent("answer.billing", {
    schema_version: 1,
    run_id: ownership.run_id,
    answer_bundle_hash: hash("5"),
    usage: {
      provider_turns: 2,
      capability_calls: 1,
      replans: 0,
      repairs: 0,
      input_tokens: 100,
      output_tokens: 20,
      evidence_bytes: 512,
    },
  }, 2);
  await handlers["answer.committed"]?.(committed);
  await handlers["answer.billing"]?.(billing);

  const committedCommand = applied[0] as AnswerCommittedProjectionV1;
  assert.equal(committedCommand.fencing_token, 3);
  assert.equal(committedCommand.run_version, 9);
  assert.equal(committedCommand.idempotency_key, committed.dedupe_key);
  assert.equal(committedCommand.sse.event_type, "done");
  assert.equal(Object.hasOwn(committedCommand.sse.data, "canonical_source"), false);
  const billingCommand = applied[1] as AnswerBillingProjectionV1;
  assert.equal(billingCommand.model_id, "glm-5.2");
  assert.equal(Object.hasOwn(billingCommand.billing, "cost"), false);
  assert.equal(Object.hasOwn(billingCommand.billing, "settlement"), false);
});

test("projection handler refuses to ACK a mismatched consumer receipt", async () => {
  const consumers = fixtureConsumers([]);
  const badConsumers: ProductProjectionConsumers = {
    ...consumers,
    async applyAnswerCommitted(command) {
      return { ...applyReceipt(command), event_payload_hash: hash("f") };
    },
  };
  const handlers = createProductProjectionHandlers(fixtureAuthority(), badConsumers);
  const handler = handlers["answer.committed"];
  assert(handler);
  await assert.rejects(
    () => handler(outboxEvent("answer.committed", {
      run_id: ownership.run_id,
      answer_bundle_hash: hash("5"),
      rendered_message_hash: hash("6"),
      memory_revision: null,
      memory_frontier_hash: null,
    })),
    /projection_apply_receipt_mismatch/,
  );
});

function rejectingMaterializers() {
  return {
    committedAnswers: {
      async materializeCommittedAnswerSource(): Promise<never> {
        throw new Error("unexpected committed answer request");
      },
    },
  };
}

function outboxEvent(eventKind: string, payload: JsonValue, id = 1): OutboxReceipt {
  return {
    outbox_id: id,
    run_id: ownership.run_id,
    event_kind: eventKind,
    dedupe_key: `${ownership.run_id}:${eventKind}`,
    payload,
    delivery_attempts: 1,
    delivery_deadline: "2026-08-02T12:00:00Z",
  };
}

function fixtureAuthority() {
  return {
    async resolveProjectionAuthority(request: ProjectionAuthorityRequestV1) {
      const claim: ProjectionAuthorityClaimV1 = {
        schema_version: 1,
        request_hash: canonicalHash(request),
        ownership,
        state: "final",
        fencing_token: 3,
        run_version: 9,
        cancel_generation: 0,
        terminal_answer_bundle_hash: hash("5"),
      };
      return claim;
    },
  };
}

function fixtureConsumers(
  applied: Array<AnswerCommittedProjectionV1 | AnswerBillingProjectionV1>,
): ProductProjectionConsumers {
  return {
    async applyAnswerCommitted(command) {
      applied.push(command);
      return applyReceipt(command);
    },
    async applyAnswerPresentation(command) {
      return applyReceipt(command);
    },
    async applyAnswerBilling(command) {
      applied.push(command);
      return applyReceipt(command);
    },
    async applyTerminal(command) {
      return applyReceipt(command);
    },
  };
}

function applyReceipt(command: {
  readonly outbox_id: number;
  readonly idempotency_key: string;
  readonly event_payload_hash: ContentHash;
  readonly ownership: AuthenticatedRunOwnershipV1;
  readonly fencing_token: number;
  readonly run_version: number;
}): ProjectionApplyReceiptV1 {
  return {
    schema_version: 1,
    outbox_id: command.outbox_id,
    idempotency_key: command.idempotency_key,
    event_payload_hash: command.event_payload_hash,
    run_id: command.ownership.run_id,
    fencing_token: command.fencing_token,
    run_version: command.run_version,
    outcome: "applied",
  };
}
