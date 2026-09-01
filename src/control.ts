import { Effect } from "effect";
import type { Command, Direction } from "./args.ts";
import {
  activeSessions,
  agentRoles,
  type Session,
  type WorkflowNode,
  type WorkflowRun,
  type WorkspaceState,
} from "./domain.ts";
import { StateError, StateStore, type StateStoreService } from "./state.ts";

export const inferredNativeId = (): string =>
  process.env.ORC_NATIVE_SESSION_ID ??
  process.env.CODEX_THREAD_ID ??
  process.env.CODEX_SESSION_ID ??
  process.env.CLAUDE_CODE_SESSION_ID ??
  process.env.CLAUDE_SESSION_ID ??
  process.env.OPENCODE_SESSION_ID ??
  `process-${process.ppid}`;

const inferredSessionId = (harness: string, nativeId: string): string => {
  const hasher = new Bun.CryptoHasher("sha256");
  hasher.update(`${harness}:\0:${nativeId}`);
  return `${harness}-${hasher.digest("hex").slice(0, 12)}`;
};

const generatedId = (prefix: string): string =>
  `${prefix}-${crypto.randomUUID().slice(0, 12)}`;

const resolveScope = (scope: string) =>
  Effect.gen(function* () {
    const store = yield* StateStore;
    return { scope: yield* store.resolveScope(scope), store };
  });

const chooseParent = (
  state: WorkspaceState,
  role: Session["role"],
  requested: string | undefined,
): string | null => {
  if (requested) return requested;
  if (role === "orchestrator") return null;
  return (
    activeSessions(state).find((session) => session.role === "orchestrator")
      ?.id ?? null
  );
};

type RegisterCommand = Extract<
  Command,
  { readonly tag: "connect" | "session-register" }
>;

const upsertSession = (
  command: RegisterCommand,
  overrides: {
    readonly directory?: string;
    readonly nativeId?: string;
    readonly traceId?: string | null;
  } = {},
): Effect.Effect<Session, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(overrides.directory ?? command.scope);
    const now = new Date().toISOString();
    const nativeId =
      overrides.nativeId ?? command.nativeId ?? inferredNativeId();
    const id =
      command.id ??
      process.env.ORC_SESSION_ID ??
      inferredSessionId(command.harness, nativeId);
    let result: Session | undefined;
    yield* resolved.store.update(resolved.scope, (state) => {
      const current = state.sessions.find(
        (session) =>
          session.id === id ||
          (session.harness === command.harness &&
            session.nativeId === nativeId),
      );
      const registration =
        command.tag === "session-register" ? command.source : "connected";
      const session: Session = {
        completion: command.completion,
        connectedAt: current?.connectedAt ?? now,
        directory: overrides.directory ?? resolved.scope,
        expectedOutput: command.expectedOutput,
        goal: command.goal,
        harness: command.harness,
        model: command.model ?? current?.model ?? null,
        id: current?.id ?? id,
        nativeId,
        nodeId:
          command.tag === "session-register" ? (command.nodeId ?? null) : null,
        parentId: chooseParent(state, command.role, command.parentId),
        purpose: command.purpose,
        registration,
        reviewBy: command.reviewBy ?? null,
        role: command.role,
        runId:
          command.tag === "session-register" ? (command.runId ?? null) : null,
        status: "working",
        successCriteria: command.successCriteria,
        title: command.title,
        traceId: overrides.traceId ?? nativeId,
        updatedAt: now,
        zmxSession: command.zmxSession ?? null,
      };
      result = session;
      return {
        ...state,
        active: true,
        sessions: [
          session,
          ...state.sessions.filter(
            (candidate) =>
              candidate.id !== session.id &&
              !(
                candidate.harness === session.harness &&
                candidate.nativeId === nativeId
              ),
          ),
        ],
        updatedAt: now,
      };
    });
    if (!result)
      return yield* new StateError({
        message: "register session produced no result",
      });
    return result;
  });

export const connect = (
  command: Extract<Command, { readonly tag: "connect" }>,
): Effect.Effect<Session, StateError, StateStoreService> =>
  upsertSession(command);

export const registerSession = (
  command: Extract<Command, { readonly tag: "session-register" }>,
  overrides?: {
    readonly directory?: string;
    readonly nativeId?: string;
    readonly traceId?: string | null;
  },
): Effect.Effect<Session, StateError, StateStoreService> =>
  upsertSession(command, overrides);

export const disconnect = (
  command: Extract<Command, { readonly tag: "disconnect" }>,
): Effect.Effect<Session, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(command.scope);
    const id = command.id ?? process.env.ORC_SESSION_ID;
    if (!id)
      return yield* new StateError({
        message: "disconnect requires a session id or ORC_SESSION_ID",
      });
    let result: Session | undefined;
    yield* resolved.store.update(resolved.scope, (state) => {
      const current = state.sessions.find((session) => session.id === id);
      if (!current) return state;
      const now = new Date().toISOString();
      result = { ...current, status: "disconnected", updatedAt: now };
      const sessions = state.sessions.map((session) =>
        session.id === id ? (result as Session) : session,
      );
      return {
        ...state,
        active: activeSessions({ ...state, sessions }).length > 0,
        sessions,
        updatedAt: now,
      };
    });
    if (!result)
      return yield* new StateError({ message: `unknown session: ${id}` });
    return result;
  });

export const readWorkspace = (
  scope: string,
): Effect.Effect<WorkspaceState, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(scope);
    return yield* resolved.store.read(resolved.scope);
  });

export const currentSession = (state: WorkspaceState): Session | undefined => {
  const explicit = process.env.ORC_SESSION_ID;
  if (explicit)
    return state.sessions.find((session) => session.id === explicit);
  const nativeId = inferredNativeId();
  const harness = process.env.ORC_AGENT;
  return state.sessions.find(
    (session) =>
      session.nativeId === nativeId &&
      (!harness || session.harness === harness),
  );
};

export const updateSessionStatus = (
  command: Extract<Command, { readonly tag: "session-update" }>,
): Effect.Effect<Session, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(command.scope);
    let result: Session | undefined;
    yield* resolved.store.update(resolved.scope, (state) => {
      const now = new Date().toISOString();
      const sessions = state.sessions.map((session) => {
        if (session.id !== command.id) return session;
        result = { ...session, status: command.status, updatedAt: now };
        return result;
      });
      return result
        ? {
            ...state,
            active: activeSessions({ ...state, sessions }).length > 0,
            sessions,
            updatedAt: now,
          }
        : state;
    });
    if (!result)
      return yield* new StateError({
        message: `unknown session: ${command.id}`,
      });
    return result;
  });

export const createRun = (
  command: Extract<Command, { readonly tag: "run-create" }>,
): Effect.Effect<WorkflowRun, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(command.scope);
    const now = new Date().toISOString();
    const state = yield* resolved.store.read(resolved.scope);
    const orchestratorId =
      command.orchestratorId ?? currentSession(state)?.id ?? null;
    const orchestrator = orchestratorId
      ? state.sessions.find((session) => session.id === orchestratorId)
      : undefined;
    const harness = command.harness ?? orchestrator?.harness ?? "unknown";
    const model = command.model ?? orchestrator?.model ?? null;
    const run: WorkflowRun = {
      agents: agentRoles.map((role) => ({ harness, model, role })),
      createdAt: now,
      edges: [],
      expectedOutput: command.expectedOutput,
      goal: command.goal,
      id: generatedId("run"),
      name: command.name,
      nodes: [],
      orchestratorId,
      status: "working",
      updatedAt: now,
    };
    yield* resolved.store.update(resolved.scope, (state) => ({
      ...state,
      active: true,
      runs: [run, ...state.runs],
      updatedAt: now,
    }));
    return run;
  });

export const setRunAgent = (
  command: Extract<Command, { readonly tag: "run-agent-set" }>,
): Effect.Effect<WorkflowRun, StateError, StateStoreService> =>
  updateRun(command.scope, command.id, (run) => ({
    ...run,
    agents: [
      {
        harness: command.harness,
        model: command.model ?? null,
        role: command.role,
      },
      ...run.agents.filter((agent) => agent.role !== command.role),
    ],
    updatedAt: new Date().toISOString(),
  }));

const updateRun = (
  scope: string,
  id: string,
  transform: (run: WorkflowRun) => WorkflowRun,
): Effect.Effect<WorkflowRun, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(scope);
    let result: WorkflowRun | undefined;
    yield* resolved.store.update(resolved.scope, (state) => {
      const runs = state.runs.map((run) => {
        if (run.id !== id) return run;
        result = transform(run);
        return result;
      });
      return result ? { ...state, runs, updatedAt: result.updatedAt } : state;
    });
    if (!result)
      return yield* new StateError({ message: `unknown run: ${id}` });
    return result;
  });

export const updateRunStatus = (
  command: Extract<Command, { readonly tag: "run-update" }>,
): Effect.Effect<WorkflowRun, StateError, StateStoreService> =>
  updateRun(command.scope, command.id, (run) => ({
    ...run,
    status: command.status,
    updatedAt: new Date().toISOString(),
  }));

export const upsertNode = (
  command: Extract<Command, { readonly tag: "node-upsert" }>,
): Effect.Effect<WorkflowNode, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const now = new Date().toISOString();
    let node: WorkflowNode | undefined;
    yield* updateRun(command.scope, command.runId, (run) => {
      const agent = run.agents.find(
        (candidate) => candidate.role === command.role,
      );
      node = {
        attempt: command.attempt,
        completion: command.completion,
        expectedOutput: command.expectedOutput,
        goal: command.goal,
        harness: command.harness || agent?.harness || "unknown",
        id: command.id,
        model: command.model ?? agent?.model ?? null,
        name: command.title,
        purpose: command.purpose,
        reviewBy: command.reviewBy ?? null,
        role: command.role,
        sessionId: command.sessionId ?? null,
        status: command.status,
        successCriteria: command.successCriteria,
        updatedAt: now,
      };
      return {
        ...run,
        edges: [
          ...run.edges.filter(
            (edge) =>
              edge.to !== node?.id || edge.relationship !== "depends-on",
          ),
          ...command.dependsOn.map((dependency) => ({
            from: dependency,
            relationship: "depends-on",
            to: node?.id ?? command.id,
          })),
        ],
        nodes: [
          node,
          ...run.nodes.filter((candidate) => candidate.id !== node?.id),
        ],
        updatedAt: now,
      };
    });
    if (!node)
      return yield* new StateError({
        message: "upsert node produced no result",
      });
    return node;
  });

export const updateNodeStatus = (
  command: Extract<Command, { readonly tag: "node-update" }>,
): Effect.Effect<WorkflowNode, StateError, StateStoreService> => {
  let result: WorkflowNode | undefined;
  return updateRun(command.scope, command.runId, (run) => {
    const now = new Date().toISOString();
    const nodes = run.nodes.map((node) => {
      if (node.id !== command.id) return node;
      result = { ...node, status: command.status, updatedAt: now };
      return result;
    });
    return { ...run, nodes, updatedAt: now };
  }).pipe(
    Effect.flatMap(() =>
      result
        ? Effect.succeed(result)
        : Effect.fail(
            new StateError({ message: `unknown node: ${command.id}` }),
          ),
    ),
  );
};

const splitFlag = (direction: Direction): string => `--${direction}`;

const spawnSplit = (
  scope: string,
  direction: Direction,
  command: ReadonlyArray<string>,
): Effect.Effect<void, StateError> =>
  Effect.tryPromise({
    try: async () => {
      const args = [
        "wezterm",
        "cli",
        "split-pane",
        "--no-auto-start",
        splitFlag(direction),
        "--cwd",
        scope,
      ];
      if (process.env.WEZTERM_PANE)
        args.push("--pane-id", process.env.WEZTERM_PANE);
      args.push("--", ...command);
      const child = Bun.spawn(args, { stderr: "inherit", stdout: "inherit" });
      const code = await child.exited;
      if (code !== 0) throw new Error(`wezterm split exited with code ${code}`);
    },
    catch: (cause) => new StateError({ message: "open WezTerm split", cause }),
  });

const selectedSession = (
  state: WorkspaceState,
  id: string,
): Effect.Effect<Session, StateError> => {
  const session = state.sessions.find((candidate) => candidate.id === id);
  return session
    ? Effect.succeed(session)
    : Effect.fail(new StateError({ message: `unknown session: ${id}` }));
};

export const attach = (
  command: Extract<Command, { readonly tag: "attach" }>,
): Effect.Effect<void, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const state = yield* readWorkspace(command.scope);
    const session = yield* selectedSession(state, command.id);
    if (!session.zmxSession)
      return yield* new StateError({
        message: `session ${session.id} has no ZMX attachment`,
      });
    yield* spawnSplit(state.scope, command.direction, [
      "zmx",
      "attach",
      session.zmxSession,
    ]);
  });

export const openTraces = (
  command: Extract<Command, { readonly tag: "traces" }>,
): Effect.Effect<void, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const state = yield* readWorkspace(command.scope);
    const session = yield* selectedSession(state, command.id);
    yield* spawnSplit(state.scope, command.direction, [
      "traces",
      "--session",
      session.traceId ?? session.nativeId,
    ]);
  });

export const launch = (
  command: Extract<Command, { readonly tag: "launch" }>,
): Effect.Effect<number, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(command.scope);
    const nativeId = crypto.randomUUID();
    const sessionId = inferredSessionId(command.harness, nativeId);
    const current = currentSession(yield* resolved.store.read(resolved.scope));
    yield* registerSession({
      completion: "orchestrator",
      expectedOutput: "A verified result",
      goal: `Run ${command.harness}`,
      harness: command.harness,
      model: command.model,
      hookInput: false,
      id: sessionId,
      nativeId,
      nodeId: undefined,
      parentId: current?.id,
      purpose: current
        ? `Child ${command.harness} session`
        : `${command.harness} orchestrator`,
      quiet: true,
      reviewBy: undefined,
      role: current ? "worker" : "orchestrator",
      runId: undefined,
      scope: resolved.scope,
      source: "managed",
      successCriteria: [],
      tag: "session-register",
      title: command.harness,
      zmxSession: command.zmxSession,
    });
    const executable = command.zmxSession
      ? [
          "zmx",
          "run",
          command.zmxSession,
          "--",
          command.harness,
          ...command.args,
        ]
      : [command.harness, ...command.args];
    return yield* Effect.tryPromise({
      try: async () => {
        const child = Bun.spawn(executable, {
          cwd: resolved.scope,
          env: {
            ...process.env,
            ORC_AGENT: command.harness,
            ...(command.model ? { ORC_MODEL: command.model } : {}),
            ORC_NATIVE_SESSION_ID: nativeId,
            ...(current ? { ORC_PARENT_SESSION_ID: current.id } : {}),
            ORC_SCOPE: resolved.scope,
            ORC_SESSION_ID: sessionId,
            ...(command.zmxSession
              ? { ORC_ZMX_SESSION: command.zmxSession }
              : {}),
          },
          stderr: "inherit",
          stdin: "inherit",
          stdout: "inherit",
        });
        return await child.exited;
      },
      catch: (cause) =>
        new StateError({ message: `launch ${command.harness}`, cause }),
    }).pipe(
      Effect.tap((code) =>
        updateSessionStatus({
          id: sessionId,
          scope: resolved.scope,
          status: code === 0 ? "done" : "failed",
          tag: "session-update",
        }),
      ),
    );
  });
