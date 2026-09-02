import { describe, expect, test } from "bun:test";
import { TerminalQueryFilter } from "./terminal-output.ts";

describe("TerminalQueryFilter", () => {
  test("removes OpenTUI capability probes", () => {
    const filter = new TerminalQueryFilter();
    const input = [
      "\u001b[?2031h",
      "\u001b]10;?\u0007",
      "\u001b]11;?\u0007",
      "\u001b[>0q",
      "\u001bP+q4d73\u001b\\",
      "\u001b[?1016$p",
      "\u001b[?u",
      "\u001b]99;i=opentui-notifications:p=?;\u001b\\",
      "\u001b]1337;Capabilities\u001b\\",
      "\u001b_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA\u001b\\",
      "\u001b[c",
      "\u001b[6n",
      "\u001b[14t",
    ].join("");
    expect(filter.write(input)).toBe("");
  });

  test("handles a query split across writes", () => {
    const filter = new TerminalQueryFilter();
    expect(filter.write("left\u001bP+q4")).toBe("left");
    expect(filter.write("d73\u001b\\right")).toBe("right");
  });

  test("preserves renderer control sequences", () => {
    const filter = new TerminalQueryFilter();
    expect(filter.write("\u001b[?1049h\u001b[31mtext\u001b[0m")).toBe(
      "\u001b[?1049h\u001b[31mtext\u001b[0m",
    );
  });
});
