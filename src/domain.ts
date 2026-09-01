import { Schema } from "effect";

export const inferredSessionId = (
  harness: string,
  nativeId: string,
): string => {
  const hasher = new Bun.CryptoHasher("sha256");
  hasher.update(`${harness}:\0:${nativeId}`);
  return `${harness}-${hasher.digest("hex").slice(0, 12)}`;
};

export const SessionRoleSchema = Schema.Literals([
  "orchestrator",
  "planner",
  "researcher",
  "implementer",
  "critic",
  "judge",
  "verifier",
  "operator",
  "generalist",
  "worker",
] as const);

export type SessionRole = typeof SessionRoleSchema.Type;

export const agentRoles: ReadonlyArray<SessionRole> = [
  "planner",
  "researcher",
  "implementer",
  "critic",
  "judge",
  "verifier",
  "operator",
  "generalist",
  "worker",
];

export const LifecycleStatusSchema = Schema.Literals([
  "queued",
  "working",
  "waiting",
  "blocked",
  "failed",
  "done",
  "cancelled",
  "disconnected",
] as const);

export type LifecycleStatus = typeof LifecycleStatusSchema.Type;

export const CompletionTargetSchema = Schema.Literals([
  "orchestrator",
  "judge",
] as const);

export type CompletionTarget = typeof CompletionTargetSchema.Type;

export const AgentConfigSchema = Schema.Struct({
  role: SessionRoleSchema,
  harness: Schema.String,
  model: Schema.NullOr(Schema.String),
});

export type AgentConfig = typeof AgentConfigSchema.Type;

export const SessionSchema = Schema.Struct({
  id: Schema.String,
  nativeId: Schema.String,
  traceId: Schema.NullOr(Schema.String),
  harness: Schema.String,
  model: Schema.NullOr(Schema.String),
  role: SessionRoleSchema,
  title: Schema.String,
  purpose: Schema.String,
  goal: Schema.String,
  expectedOutput: Schema.String,
  successCriteria: Schema.Array(Schema.String),
  completion: CompletionTargetSchema,
  reviewBy: Schema.NullOr(Schema.String),
  parentId: Schema.NullOr(Schema.String),
  runId: Schema.NullOr(Schema.String),
  nodeId: Schema.NullOr(Schema.String),
  providerRef: Schema.NullOr(Schema.String),
  directory: Schema.String,
  registration: Schema.Literals(["connected", "hook", "managed"] as const),
  status: LifecycleStatusSchema,
  connectedAt: Schema.String,
  updatedAt: Schema.String,
});

export type Session = typeof SessionSchema.Type;

export const WorkflowNodeSchema = Schema.Struct({
  id: Schema.String,
  name: Schema.String,
  purpose: Schema.String,
  role: SessionRoleSchema,
  harness: Schema.String,
  model: Schema.NullOr(Schema.String),
  goal: Schema.String,
  expectedOutput: Schema.String,
  successCriteria: Schema.Array(Schema.String),
  completion: CompletionTargetSchema,
  reviewBy: Schema.NullOr(Schema.String),
  sessionId: Schema.NullOr(Schema.String),
  status: LifecycleStatusSchema,
  attempt: Schema.Number,
  updatedAt: Schema.String,
});

export type WorkflowNode = typeof WorkflowNodeSchema.Type;

export const WorkflowEdgeSchema = Schema.Struct({
  from: Schema.String,
  to: Schema.String,
  relationship: Schema.String,
});

export type WorkflowEdge = typeof WorkflowEdgeSchema.Type;

export const WorkflowRunSchema = Schema.Struct({
  id: Schema.String,
  name: Schema.String,
  goal: Schema.String,
  expectedOutput: Schema.String,
  status: LifecycleStatusSchema,
  orchestratorId: Schema.NullOr(Schema.String),
  agents: Schema.Array(AgentConfigSchema),
  nodes: Schema.Array(WorkflowNodeSchema),
  edges: Schema.Array(WorkflowEdgeSchema),
  createdAt: Schema.String,
  updatedAt: Schema.String,
});

export type WorkflowRun = typeof WorkflowRunSchema.Type;

export const WorkspaceStateSchema = Schema.Struct({
  schemaVersion: Schema.Literal("orc.state/v2"),
  scope: Schema.String,
  active: Schema.Boolean,
  updatedAt: Schema.String,
  sessions: Schema.Array(SessionSchema),
  runs: Schema.Array(WorkflowRunSchema),
});

export type WorkspaceState = typeof WorkspaceStateSchema.Type;

export const emptyWorkspace = (scope: string): WorkspaceState => ({
  schemaVersion: "orc.state/v2",
  scope,
  active: false,
  updatedAt: new Date(0).toISOString(),
  sessions: [],
  runs: [],
});

export const activeSessions = (state: WorkspaceState): ReadonlyArray<Session> =>
  state.sessions.filter(
    (session) =>
      session.status !== "done" &&
      session.status !== "failed" &&
      session.status !== "cancelled" &&
      session.status !== "disconnected",
  );

export const sessionsByRecency = (
  sessions: ReadonlyArray<Session>,
): ReadonlyArray<Session> =>
  [...sessions].sort((left, right) =>
    right.updatedAt.localeCompare(left.updatedAt),
  );

export const runsByRecency = (
  runs: ReadonlyArray<WorkflowRun>,
): ReadonlyArray<WorkflowRun> =>
  [...runs].sort((left, right) =>
    right.updatedAt.localeCompare(left.updatedAt),
  );
