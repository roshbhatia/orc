import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Effect } from "effect";
import { connect } from "./control.ts";
import { handleMcpRequest } from "./mcp.ts";
import { StateStoreLive } from "./state.ts";

const saved = {
  orcScope: process.env.ORC_SCOPE,
  orcSession: process.env.ORC_SESSION_ID,
  stateHome: process.env.XDG_STATE_HOME,
};

afterEach(() => {
  for (const [name, value] of [
    ["ORC_SCOPE", saved.orcScope],
    ["ORC_SESSION_ID", saved.orcSession],
    ["XDG_STATE_HOME", saved.stateHome],
  ] as const) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
});

describe("MCP", () => {
  test("hides tools outside an Orc session", async () => {
    delete process.env.ORC_SCOPE;
    const output = await handleMcpRequest({
      id: 1,
      jsonrpc: "2.0",
      method: "tools/list",
    });
    expect(JSON.parse(output ?? "").result.tools).toEqual([]);
  });

  test("exposes tools to the registered session", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-mcp-test-"));
    process.env.XDG_STATE_HOME = directory;
    process.env.ORC_SCOPE = directory;
    process.env.ORC_SESSION_ID = "orc-test";
    try {
      await Effect.runPromise(
        connect({
          completion: "orchestrator",
          expectedOutput: "verified result",
          goal: "test MCP gating",
          harness: "codex",
          id: "orc-test",
          nativeId: "native-test",
          parentId: undefined,
          purpose: "MCP test",
          reviewBy: undefined,
          role: "orchestrator",
          scope: directory,
          successCriteria: [],
          tag: "connect",
          title: "MCP test",
          zmxSession: undefined,
        }).pipe(Effect.provide(StateStoreLive)),
      );
      const output = await handleMcpRequest({
        id: 1,
        jsonrpc: "2.0",
        method: "tools/list",
      });
      const names = JSON.parse(output ?? "").result.tools.map(
        (tool: { readonly name: string }) => tool.name,
      );
      expect(names).toContain("orc_run_create");
      expect(names).toContain("orc_node_upsert");
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });
});
