import type { OutboxReceipt } from "./contracts.js";
import { canonicalHash } from "./json.js";
import { ContractViolation } from "./validation.js";

export type ProductOutboxKind =
  | "answer.committed"
  | "answer.presentation"
  | "answer.billing"
  | "answer.notification"
  | "run.cancelled"
  | "run.failed"
  | "run.deferred";

export type ProductOutboxHandlers = Readonly<
  Partial<Record<ProductOutboxKind, (event: OutboxReceipt) => Promise<void>>>
>;

export interface DispatchOnceOptions {
  readonly workerId: string;
  readonly claimLimit: number;
  readonly leaseMs: number;
  readonly concurrency: number;
}

export interface DispatchReport {
  readonly claimed: number;
  readonly delivered: number;
  readonly released: number;
  readonly ackFailures: number;
}

export interface OutboxClientPort {
  claim(workerId: string, limit: number, leaseMs: number): Promise<readonly OutboxReceipt[]>;
  ack(
    workerId: string,
    outboxId: number,
    result: { readonly success: true } | { readonly success: false; readonly errorHash: string },
  ): Promise<"delivered" | "already_delivered" | "released" | "already_released">;
}

/**
 * One bounded outbox pass. Scheduling/backoff belongs to the product worker,
 * so this helper creates no resident timers or per-session tasks.
 */
export async function dispatchOutboxOnce(
  client: OutboxClientPort,
  handlers: ProductOutboxHandlers,
  options: DispatchOnceOptions,
): Promise<DispatchReport> {
  if (!Number.isSafeInteger(options.concurrency) || options.concurrency < 1 || options.concurrency > 16) {
    throw new ContractViolation("invalid_outbox_concurrency");
  }
  const events = await client.claim(options.workerId, options.claimLimit, options.leaseMs);
  let cursor = 0;
  let delivered = 0;
  let released = 0;
  let ackFailures = 0;
  const workerCount = Math.min(options.concurrency, events.length);

  await Promise.all(
    Array.from({ length: workerCount }, async () => {
      for (;;) {
        const index = cursor;
        cursor += 1;
        const event = events[index];
        if (!event) return;

        const handler = isProductOutboxKind(event.event_kind)
          ? handlers[event.event_kind]
          : undefined;
        try {
          if (!handler) throw new ContractViolation("unsupported_outbox_event");
          await handler(event);
          try {
            await client.ack(options.workerId, event.outbox_id, { success: true });
            delivered += 1;
          } catch {
            ackFailures += 1;
          }
        } catch (error) {
          const errorHash = canonicalHash(redactedFailureClass(error));
          try {
            await client.ack(options.workerId, event.outbox_id, {
              success: false,
              errorHash,
            });
            released += 1;
          } catch {
            ackFailures += 1;
          }
        }
      }
    }),
  );

  return { claimed: events.length, delivered, released, ackFailures };
}

function isProductOutboxKind(value: string): value is ProductOutboxKind {
  return PRODUCT_OUTBOX_KINDS.has(value as ProductOutboxKind);
}

function redactedFailureClass(error: unknown) {
  return {
    class:
      error instanceof Error && error.name.length > 0 && error.name.length <= 128
        ? error.name
        : "UnknownOutboxFailure",
  } as const;
}

const PRODUCT_OUTBOX_KINDS = new Set<ProductOutboxKind>([
  "answer.committed",
  "answer.presentation",
  "answer.billing",
  "answer.notification",
  "run.cancelled",
  "run.failed",
  "run.deferred",
]);
