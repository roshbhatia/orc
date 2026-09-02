import { describe, expect, test } from "bun:test";
import { Effect, Exit } from "effect";
import { parseArgs } from "./args.ts";

describe("parseArgs", () => {
  test("opens the TUI by default", () => {
    const command = Effect.runSync(parseArgs([]));
    expect(command.tag).toBe("tui");
  });

  test("parses Fish completion generation", () => {
    expect(Effect.runSync(parseArgs(["completion", "fish"]))).toEqual({
      shell: "fish",
      tag: "completion",
    });
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

  test("parses directory-scoped session lifecycle commands", () => {
    expect(
      Effect.runSync(
        parseArgs([
          "session",
          "adopt",
          "--scope",
          "/workspace",
          "--harness",
          "codex",
          "--title",
          "Current task",
        ]),
      ),
    ).toMatchObject({
      harness: "codex",
      role: "orchestrator",
      scope: "/workspace",
      tag: "session-adopt",
      title: "Current task",
    });
    expect(
      Effect.runSync(
        parseArgs([
          "session",
          "archive",
          "--scope",
          "/workspace",
          "--native-id",
          "native-session",
          "--quiet",
        ]),
      ),
    ).toMatchObject({
      nativeId: "native-session",
      quiet: true,
      scope: "/workspace",
      tag: "session-archive",
    });
  });

  test("parses provider validation", () => {
    expect(
      Effect.runSync(
        parseArgs([
          "provider",
          "validate",
          "wezterm",
          "--scope",
          "/workspace",
          "--json",
        ]),
      ),
    ).toEqual({
      json: true,
      name: "wezterm",
      scope: "/workspace",
      tag: "provider-validate",
    });
  });
});
