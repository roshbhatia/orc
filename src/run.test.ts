import { describe, expect, test } from "bun:test";
import { mkdtemp, realpath, rm } from "node:fs/promises";
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
  test("generates Fish completions from the command catalog", async () => {
    const output = capture();
    expect(
      await Effect.runPromise(
        run(["completion", "fish"], output.streams, "test"),
      ),
    ).toBe(0);
    expect(output.stdout.join("\n")).toContain(
      "complete -c orc -n '__fish_use_subcommand' -a 'connect'",
    );
    expect(output.stdout.join("\n")).toContain("-l role");
  });

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
      const runId = created.stdout[0] ?? "";
      expect(runId).toMatch(/^run-/);

      const createdJson = capture();
      expect(
        await Effect.runPromise(
          run(
            [
              "run",
              "create",
              "--scope",
              directory,
              "--name",
              "JSON run",
              "--json",
            ],
            createdJson.streams,
            "test",
          ),
        ),
      ).toBe(0);
      expect(JSON.parse(createdJson.stdout[0] ?? "")).toMatchObject({
        name: "JSON run",
      });

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

  test("adopts a new directory-bound orchestrator and archives the old one", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-adopt-test-"));
    const previousStateHome = process.env.XDG_STATE_HOME;
    const previousSession = process.env.CODEX_SESSION_ID;
    const previousThread = process.env.CODEX_THREAD_ID;
    const previousProviderDirectory = process.env.ORC_PROVIDER_DIR;
    process.env.XDG_STATE_HOME = directory;
    process.env.CODEX_SESSION_ID = "native-current";
    process.env.CODEX_THREAD_ID = "native-current";
    process.env.ORC_PROVIDER_DIR = join(directory, "providers");
    try {
      const original = capture();
      expect(
        await Effect.runPromise(
          run(
            [
              "connect",
              "--scope",
              directory,
              "--id",
              "old-orchestrator",
              "--harness",
              "codex",
              "--role",
              "orchestrator",
            ],
            original.streams,
            "test",
          ),
        ),
      ).toBe(0);

      const adopted = capture();
      expect(
        await Effect.runPromise(
          run(
            [
              "session",
              "adopt",
              "--scope",
              directory,
              "--harness",
              "codex",
              "--title",
              "Current orchestrator",
            ],
            adopted.streams,
            "test",
          ),
        ),
      ).toBe(0);
      expect(adopted.stdout[0]).toMatch(/^codex-[a-f0-9]{12}-[a-f0-9]{6}$/);

      const listed = capture();
      expect(
        await Effect.runPromise(
          run(
            ["session", "list", "--scope", directory, "--json"],
            listed.streams,
            "test",
          ),
        ),
      ).toBe(0);
      const sessions = JSON.parse(listed.stdout[0] ?? "") as ReadonlyArray<{
        readonly directory: string;
        readonly status: string;
        readonly title: string;
      }>;
      expect(sessions).toEqual(
        expect.arrayContaining([
          expect.objectContaining({ status: "archived" }),
          expect.objectContaining({
            directory: await realpath(directory),
            status: "working",
            title: "Current orchestrator",
          }),
        ]),
      );
    } finally {
      if (previousStateHome === undefined) delete process.env.XDG_STATE_HOME;
      else process.env.XDG_STATE_HOME = previousStateHome;
      if (previousSession === undefined) delete process.env.CODEX_SESSION_ID;
      else process.env.CODEX_SESSION_ID = previousSession;
      if (previousThread === undefined) delete process.env.CODEX_THREAD_ID;
      else process.env.CODEX_THREAD_ID = previousThread;
      if (previousProviderDirectory === undefined)
        delete process.env.ORC_PROVIDER_DIR;
      else process.env.ORC_PROVIDER_DIR = previousProviderDirectory;
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("does not flash-close a pane for a live unmanaged session", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-attach-test-"));
    const previousStateHome = process.env.XDG_STATE_HOME;
    process.env.XDG_STATE_HOME = directory;
    try {
      const registered = capture();
      expect(
        await Effect.runPromise(
          run(
            [
              "session",
              "register",
              "--scope",
              directory,
              "--id",
              "live-session",
              "--native-id",
              "native-live",
              "--harness",
              "codex",
              "--source",
              "hook",
            ],
            registered.streams,
            "test",
          ),
        ),
      ).toBe(0);
      const attached = capture();
      expect(
        await Effect.runPromise(
          run(
            ["attach", "live-session", "--scope", directory],
            attached.streams,
            "test",
          ),
        ),
      ).toBe(1);
      expect(attached.stderr.join("\n")).toContain(
        "session is active outside a persistence provider",
      );
    } finally {
      if (previousStateHome === undefined) delete process.env.XDG_STATE_HOME;
      else process.env.XDG_STATE_HOME = previousStateHome;
      await rm(directory, { force: true, recursive: true });
    }
  });
});
