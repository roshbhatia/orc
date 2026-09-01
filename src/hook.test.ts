import { describe, expect, test } from "bun:test";
import { parseHookContext } from "./hook.ts";

describe("parseHookContext", () => {
  test("accepts harness session fields", () => {
    expect(
      parseHookContext({
        cwd: "/workspace",
        prompt: "Build the control plane",
        session_id: "native-1",
      }),
    ).toMatchObject({
      directory: "/workspace",
      goal: "Build the control plane",
      nativeId: "native-1",
      traceId: "native-1",
    });
  });
});
