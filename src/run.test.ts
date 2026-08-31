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
});
