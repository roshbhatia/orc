import { describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { Effect } from "effect";
import { inferredSessionId } from "./domain.ts";
import { StateStore, StateStoreLive, statePath } from "./state.ts";

describe("statePath", () => {
  test("uses a stable bounded scope key", () => {
    const first = statePath("/workspace/project");
    const second = statePath("/workspace/project");
    expect(first).toBe(second);
    expect(first.endsWith(".json")).toBe(true);
    expect(first).not.toContain("/workspace/project.json");
  });

  test("repairs an empty session id from stable harness fields", async () => {
    const stateHome = await mkdtemp(join(tmpdir(), "orc-state-test-"));
    const previousStateHome = process.env.XDG_STATE_HOME;
    const scope = "/workspace/project";
    const nativeId = "native-session";
    process.env.XDG_STATE_HOME = stateHome;
    try {
      const target = statePath(scope);
      await mkdir(dirname(target), { recursive: true });
      await writeFile(
        target,
        JSON.stringify({
          active: true,
          runs: [],
          schemaVersion: "orc.state/v2",
          scope,
          sessions: [
            {
              completion: "orchestrator",
              connectedAt: "2026-01-01T00:00:00.000Z",
              directory: scope,
              expectedOutput: "A verified result",
              goal: "Complete the assigned work",
              harness: "codex",
              id: "",
              model: null,
              nativeId,
              nodeId: null,
              parentId: null,
              providerRef: null,
              purpose: "Agent session",
              registration: "hook",
              reviewBy: null,
              role: "orchestrator",
              runId: null,
              status: "working",
              successCriteria: [],
              title: "Agent session",
              traceId: nativeId,
              updatedAt: "2026-01-01T00:00:00.000Z",
            },
          ],
          updatedAt: "2026-01-01T00:00:00.000Z",
        }),
      );
      const state = await Effect.runPromise(
        Effect.gen(function* () {
          return yield* (yield* StateStore).read(scope);
        }).pipe(Effect.provide(StateStoreLive)),
      );
      expect(state.sessions[0]?.id).toBe(inferredSessionId("codex", nativeId));
    } finally {
      if (previousStateHome === undefined) delete process.env.XDG_STATE_HOME;
      else process.env.XDG_STATE_HOME = previousStateHome;
      await rm(stateHome, { force: true, recursive: true });
    }
  });
});
