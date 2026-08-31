import { describe, expect, test } from "bun:test";
import { type Session, sessionsByRecency } from "./domain.ts";

const session = (id: string, updatedAt: string): Session => ({
  completion: "orchestrator",
  connectedAt: updatedAt,
  expectedOutput: "verified result",
  goal: "complete work",
  harness: "codex",
  id,
  nativeId: id,
  parentId: null,
  purpose: id,
  role: "worker",
  status: "working",
  updatedAt,
  zmxSession: null,
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
