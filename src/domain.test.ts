import { describe, expect, test } from "bun:test";
import { type Session, sessionsByRecency } from "./domain.ts";

const session = (id: string, updatedAt: string): Session => ({
  completion: "orchestrator",
  connectedAt: updatedAt,
  directory: "/workspace",
  expectedOutput: "verified result",
  goal: "complete work",
  harness: "codex",
  model: null,
  id,
  nativeId: id,
  nodeId: null,
  parentId: null,
  purpose: id,
  registration: "connected",
  reviewBy: null,
  role: "worker",
  runId: null,
  status: "working",
  successCriteria: [],
  title: id,
  traceId: id,
  updatedAt,
  providerRef: null,
});

describe("sessionsByRecency", () => {
  test("places recent sessions first", () => {
    const older = session("older", "2026-08-30T00:00:00.000Z");
    const newer = session("newer", "2026-08-31T00:00:00.000Z");
    expect(sessionsByRecency([older, newer]).map(({ id }) => id)).toEqual([
      "newer",
      "older",
    ]);
  });
});
