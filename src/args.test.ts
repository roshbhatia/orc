import { describe, expect, test } from "bun:test";
import { Effect, Exit } from "effect";
import { parseArgs } from "./args.ts";

describe("parseArgs", () => {
  test("parses JSON output", () => {
    expect(Effect.runSync(parseArgs(["--json", "Roshan"]))).toEqual({
      json: true,
      name: "Roshan",
      version: false,
    });
  });

  test("rejects unknown options", () => {
    expect(Exit.isFailure(Effect.runSyncExit(parseArgs(["--unknown"])))).toBe(
      true,
    );
  });
});
