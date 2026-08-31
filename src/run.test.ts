import { describe, expect, test } from "bun:test";
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
  test("writes text", () => {
    const output = capture();
    expect(Effect.runSync(run(["Roshan"], output.streams, "test"))).toBe(0);
    expect(output.stdout).toEqual(["hello, Roshan"]);
    expect(output.stderr).toEqual([]);
  });

  test("writes JSON", () => {
    const output = capture();
    expect(
      Effect.runSync(run(["--json", "Roshan"], output.streams, "test")),
    ).toBe(0);
    expect(JSON.parse(output.stdout[0] ?? "")).toEqual({
      message: "hello, Roshan",
      status: "done",
    });
  });
});
