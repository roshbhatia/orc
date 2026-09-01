import { describe, expect, test } from "bun:test";
import { ansiToStyledText } from "./ansi.ts";

describe("ansiToStyledText", () => {
  test("preserves indexed color and strips control sequences", () => {
    const styled = ansiToStyledText("plain \u001b[31mred\u001b[0m text");
    expect(styled.chunks.map((chunk) => chunk.text).join("")).toBe(
      "plain red text",
    );
    expect(styled.chunks[1]?.fg?.slot).toBe(1);
    expect(styled.chunks[2]?.fg).toBeUndefined();
  });
});
