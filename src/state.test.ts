import { describe, expect, test } from "bun:test";
import { statePath } from "./state.ts";

describe("statePath", () => {
  test("uses a stable bounded scope key", () => {
    const first = statePath("/workspace/project");
    const second = statePath("/workspace/project");
    expect(first).toBe(second);
    expect(first.endsWith(".json")).toBe(true);
    expect(first).not.toContain("/workspace/project.json");
  });
});
