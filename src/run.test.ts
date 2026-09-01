import { describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Effect } from "effect";
import { run } from "./run.ts";

const capture = () => {
  const stdout: Array<string> = [];
  const stderr: Array<string> = [];
  return {
    stderr,
    stdout,
    streams: {
      stderr: (value: string) => stderr.push(value),
      stdout: (value: string) => stdout.push(value),
    },
  };
};

describe("run", () => {
  test("connects, lists, and disconnects one session", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-run-test-"));
    const previousStateHome = process.env.XDG_STATE_HOME;
    process.env.XDG_STATE_HOME = directory;
    try {
      const connected = capture();
      expect(
        await Effect.runPromise(
          run(
            [
              "connect",
              "--scope",
              directory,
              "--id",
              "session-test",
              "--harness",
              "codex",
              "--role",
              "orchestrator",
              "--purpose",
              "verify lifecycle",
            ],
            connected.streams,
            "test",
          ),
        ),
      ).toBe(0);
      expect(connected.stdout).toEqual(["session-test"]);

      const listed = capture();
      expect(
        await Effect.runPromise(
          run(["list", "--scope", directory, "--json"], listed.streams, "test"),
        ),
      ).toBe(0);
      expect(JSON.parse(listed.stdout[0] ?? "")).toMatchObject([
        { id: "session-test", status: "working" },
      ]);

      const disconnected = capture();
      expect(
        await Effect.runPromise(
          run(
            ["disconnect", "session-test", "--scope", directory],
            disconnected.streams,
            "test",
          ),
        ),
      ).toBe(0);

      const status = capture();
      expect(
        await Effect.runPromise(
          run(
            ["status", "--scope", directory, "--json"],
            status.streams,
            "test",
          ),
        ),
      ).toBe(0);
      expect(JSON.parse(status.stdout[0] ?? "")).toMatchObject({
        active: false,
        working: 0,
      });
    } finally {
      if (previousStateHome === undefined) {
        delete process.env.XDG_STATE_HOME;
      } else {
        process.env.XDG_STATE_HOME = previousStateHome;
      }
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("creates a workflow graph with contracts", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-graph-test-"));
    const previousStateHome = process.env.XDG_STATE_HOME;
    process.env.XDG_STATE_HOME = directory;
    try {
      const created = capture();
      expect(
        await Effect.runPromise(
          run(
            [
              "run",
              "create",
              "--scope",
              directory,
              "--name",
              "Ship Orc",
              "--goal",
              "Verify parity",
              "--expected-output",
              "A released CLI",
              "--harness",
              "codex",
              "--model",
              "gpt-5.6",
            ],
            created.streams,
            "test",
          ),
        ),
      ).toBe(0);
      const runId = JSON.parse(created.stdout[0] ?? "").id as string;

      const configured = capture();
      expect(
        await Effect.runPromise(
          run(
            [
              "run",
              "agent",
              runId,
              "--scope",
              directory,
              "--role",
              "researcher",
              "--harness",
              "claude",
              "--model",
              "opus",
            ],
            configured.streams,
            "test",
          ),
        ),
      ).toBe(0);

      const node = capture();
      expect(
        await Effect.runPromise(
          run(
            [
              "node",
              "upsert",
              "implement",
              "--scope",
              directory,
              "--run",
              runId,
              "--title",
              "Implement parity",
              "--role",
              "implementer",
              "--goal",
              "Match the control API",
              "--expected-output",
              "Passing tests",
              "--success",
              "MCP tools are gated",
              "--status",
              "working",
            ],
            node.streams,
            "test",
          ),
        ),
      ).toBe(0);

      const shown = capture();
      expect(
        await Effect.runPromise(
          run(
            ["run", "show", runId, "--scope", directory, "--json"],
            shown.streams,
            "test",
          ),
        ),
      ).toBe(0);
      expect(JSON.parse(shown.stdout[0] ?? "")).toMatchObject({
        goal: "Verify parity",
        nodes: [
          {
            harness: "codex",
            id: "implement",
            model: "gpt-5.6",
            role: "implementer",
            successCriteria: ["MCP tools are gated"],
          },
        ],
        agents: expect.arrayContaining([
          { harness: "codex", model: "gpt-5.6", role: "implementer" },
          { harness: "claude", model: "opus", role: "researcher" },
        ]),
      });
    } finally {
      if (previousStateHome === undefined) {
        delete process.env.XDG_STATE_HOME;
      } else {
        process.env.XDG_STATE_HOME = previousStateHome;
      }
      await rm(directory, { force: true, recursive: true });
    }
  });
});
