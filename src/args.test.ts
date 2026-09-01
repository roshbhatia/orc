import { describe, expect, test } from "bun:test";
import { Effect, Exit } from "effect";
import { parseArgs } from "./args.ts";

describe("parseArgs", () => {
  test("opens the TUI by default", () => {
    const command = Effect.runSync(parseArgs([]));
    expect(command.tag).toBe("tui");
  });

  test("parses a session contract", () => {
    expect(
      Effect.runSync(
        parseArgs([
          "connect",
          "--harness",
          "codex",
          "--role",
          "researcher",
          "--purpose",
          "inspect storage",
          "--goal",
          "find migration risks",
          "--expected-output",
          "risk list",
          "--completion",
          "judge",
        ]),
      ),
    ).toMatchObject({
      completion: "judge",
      expectedOutput: "risk list",
      goal: "find migration risks",
      harness: "codex",
      purpose: "inspect storage",
      role: "researcher",
      tag: "connect",
    });
  });

  test("rejects unknown roles", () => {
    expect(
      Exit.isFailure(
        Effect.runSyncExit(parseArgs(["connect", "--role", "manager"])),
      ),
    ).toBe(true);
  });

  test("parses provider actions without host tool vocabulary", () => {
    expect(
      Effect.runSync(
        parseArgs([
          "launch",
          "codex",
          "--managed",
          "agent-session",
          "--",
          "--resume",
        ]),
      ),
    ).toMatchObject({
      args: ["--resume"],
      harness: "codex",
      managedId: "agent-session",
      tag: "launch",
    });
    expect(
      Effect.runSync(
        parseArgs(["inspect", "session-id", "--direction", "left"]),
      ),
    ).toMatchObject({
      direction: "left",
      id: "session-id",
      tag: "inspect",
    });
  });
});
