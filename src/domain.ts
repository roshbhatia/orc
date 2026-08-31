import { Schema } from "effect";

export const SessionRoleSchema = Schema.Literals([
  "orchestrator",
  "planner",
  "researcher",
  "implementer",
  "judge",
  "worker",
] as const);

export type SessionRole = typeof SessionRoleSchema.Type;

export const SessionStatusSchema = Schema.Literals([
  "working",
  "waiting",
  "blocked",
  "failed",
  "done",
  "disconnected",
] as const);

export type SessionStatus = typeof SessionStatusSchema.Type;

export const SessionSchema = Schema.Struct({
  id: Schema.String,
  nativeId: Schema.String,
  harness: Schema.String,
  role: SessionRoleSchema,
  purpose: Schema.String,
  goal: Schema.String,
  expectedOutput: Schema.String,
  completion: Schema.Literals(["orchestrator", "judge"] as const),
  parentId: Schema.NullOr(Schema.String),
  zmxSession: Schema.NullOr(Schema.String),
  status: SessionStatusSchema,
  connectedAt: Schema.String,
  updatedAt: Schema.String,
});

export type Session = typeof SessionSchema.Type;

export const WorkspaceStateSchema = Schema.Struct({
  schemaVersion: Schema.Literal("orc.state/v1"),
  scope: Schema.String,
  active: Schema.Boolean,
  updatedAt: Schema.String,
  sessions: Schema.Array(SessionSchema),
});

export type WorkspaceState = typeof WorkspaceStateSchema.Type;

export const emptyWorkspace = (scope: string): WorkspaceState => ({
  schemaVersion: "orc.state/v1",
  scope,
  active: false,
  updatedAt: new Date(0).toISOString(),
  sessions: [],
});

export const activeSessions = (state: WorkspaceState): ReadonlyArray<Session> =>
  state.sessions.filter(
    (session) =>
      session.status !== "done" &&
      session.status !== "failed" &&
      session.status !== "disconnected",
  );

export const sessionsByRecency = (
  sessions: ReadonlyArray<Session>,
): ReadonlyArray<Session> =>
  [...sessions].sort((left, right) =>
    right.updatedAt.localeCompare(left.updatedAt),
  );
