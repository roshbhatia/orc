import { describe, expect, test } from "bun:test";
import type { Session, WorkflowRun, WorkspaceState } from "./domain.ts";
import { explorerRows, graphLevels, moveGraphSelection } from "./tui-model.ts";

const session = (id: string, role: Session["role"]): Session => ({
  completion: "orchestrator",
  connectedAt: "2026-09-01T00:00:00.000Z",
  directory: "/workspace",
  expectedOutput: "verified result",
  goal: `${id} goal`,
  harness: "codex",
  id,
  model: null,
  nativeId: `${id}-native`,
  nodeId: null,
  parentId: null,
  providerRef: null,
  providers: [],
  purpose: `${id} purpose`,
  registration: "connected",
  reviewBy: null,
  role,
  runId: null,
  status: "working",
  successCriteria: [],
  title: id,
  traceId: `${id}-native`,
  updatedAt: "2026-09-01T00:00:00.000Z",
});

const run: WorkflowRun = {
  agents: [],
  createdAt: "2026-09-01T00:00:00.000Z",
  edges: [{ from: "plan", relationship: "feeds", to: "build" }],
  expectedOutput: "working code",
  goal: "ship it",
  id: "run-1",
  name: "Ship Orc",
  nodes: [
    {
      attempt: 1,
      completion: "orchestrator",
      expectedOutput: "plan",
      goal: "plan",
      harness: "codex",
      id: "plan",
      model: null,
      name: "Plan",
      purpose: "remove uncertainty",
      reviewBy: null,
      role: "planner",
      sessionId: null,
      status: "done",
      successCriteria: [],
      updatedAt: "2026-09-01T00:00:00.000Z",
    },
    {
      attempt: 1,
      completion: "orchestrator",
      expectedOutput: "code",
      goal: "build",
      harness: "codex",
      id: "build",
      model: null,
      name: "Build",
      purpose: "implement",
      reviewBy: null,
      role: "implementer",
      sessionId: null,
      status: "working",
      successCriteria: [],
      updatedAt: "2026-09-01T00:00:00.000Z",
    },
  ],
  orchestratorId: "root",
  status: "working",
  updatedAt: "2026-09-01T00:00:00.000Z",
};

const state: WorkspaceState = {
  active: true,
  runs: [run],
  schemaVersion: "orc.state/v3",
  scope: "/workspace",
  sessions: [session("root", "orchestrator")],
  updatedAt: "2026-09-01T00:00:00.000Z",
};

describe("TUI model", () => {
  test("nests a workflow under its orchestrator", () => {
    expect(explorerRows(state).map(({ depth, kind }) => [kind, depth])).toEqual(
      [
        ["session", 0],
        ["run", 1],
        ["node", 2],
        ["node", 2],
      ],
    );
  });

  test("lays graph nodes out after their dependencies", () => {
    const levels = graphLevels(state, run);
    expect(levels.map((level) => level.map(({ id }) => id))).toEqual([
      ["orchestrator:run-1"],
      ["node:plan"],
      ["node:build"],
    ]);
    expect(moveGraphSelection(levels, "node:plan", "down")).toBe("node:build");
  });

  test("hides archived sessions until requested", () => {
    const archived = {
      ...session("old", "orchestrator"),
      status: "archived" as const,
    };
    const withArchive = { ...state, sessions: [archived, ...state.sessions] };
    expect(explorerRows(withArchive).map((row) => row.id)).not.toContain("old");
    expect(explorerRows(withArchive, true).map((row) => row.id)).toContain(
      "old",
    );
  });
});
