import { describe, expect, test } from "bun:test";
import { fullKeyHelp, keyHelp, tuiActionFor } from "./keymap.ts";

describe("keymap", () => {
  test("uses one binding source for input and help", () => {
    expect(tuiActionFor({ name: "j" })).toBe("next");
    expect(keyHelp(["next"])).toBe("j/down next");
    expect(fullKeyHelp()).toContain("j/down       next");
  });

  test("matches modified keys exactly", () => {
    expect(tuiActionFor({ name: "tab", shift: true })).toBe("tab-previous");
    expect(tuiActionFor({ name: "tab" })).toBe("tab-next");
  });
});
