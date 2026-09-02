import { describe, expect, test } from "bun:test";
import { ansiToStyledText, sanitizeTerminalText } from "./ansi.ts";

describe("ansiToStyledText", () => {
  test("preserves indexed color and strips control sequences", () => {
    const styled = ansiToStyledText("plain \u001b[31mred\u001b[0m text");
    expect(styled.chunks.map((chunk) => chunk.text).join("")).toBe(
      "plain red text",
    );
    expect(styled.chunks[1]?.fg?.slot).toBe(1);
    expect(styled.chunks[2]?.fg).toBeUndefined();
  });

  test("removes terminal capability queries from external output", () => {
    const input = [
      "before",
      "\u001b[>0q",
      "\u001bP+q4d73\u001b\\",
      "\u001b_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA\u001b\\",
      "\u001b]11;?\u0007",
      "after",
    ].join("");
    expect(sanitizeTerminalText(input)).toBe("beforeafter");
  });

  test("preserves SGR while removing other controls", () => {
    expect(sanitizeTerminalText("\u001b[31mred\u001b[0m\u0000 text")).toBe(
      "\u001b[31mred\u001b[0m text",
    );
  });
});
