import { Effect } from "effect";
import type { Command } from "./args.ts";
import {
  activeSessions,
  agentRoles,
  inferredSessionId,
  type Session,
  sessionsByRecency,
  type WorkflowNode,
  type WorkflowRun,
  type WorkspaceState,
} from "./domain.ts";
import {
  describeSession,
  discoverSessionBindings,
  invokeProvider,
  providerOutput,
  resolveProviderChain,
} from "./provider.ts";
import { StateError, StateStore, type StateStoreService } from "./state.ts";

export const inferredNativeId = (): string =>
  process.env.ORC_NATIVE_SESSION_ID ??
  process.env.CODEX_THREAD_ID ??
  process.env.CODEX_SESSION_ID ??
  process.env.CLAUDE_CODE_SESSION_ID ??
  process.env.CLAUDE_SESSION_ID ??
  process.env.OPENCODE_SESSION_ID ??
  `process-${process.ppid}`;

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
    const incarnationId = `${id}-${crypto.randomUUID().slice(0, 6)}`;
    let result: Session | undefined;
    yield* resolved.store.update(resolved.scope, (state) => {
      const current = state.sessions.find(
        (session) =>
          session.status !== "archived" &&
          (session.id === id ||
            (session.harness === command.harness &&
              session.nativeId === nativeId)),
      );
      const selectedId =
        current?.id ??
        (state.sessions.some((session) => session.id === id)
          ? incarnationId
          : id);
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
        id: selectedId,
        nativeId,
        nodeId:
          command.tag === "session-register" ? (command.nodeId ?? null) : null,
        parentId: chooseParent(state, command.role, command.parentId),
        purpose: command.purpose,
        providers: current?.providers ?? [],
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
        providerRef: command.providerRef ?? null,
      };
      result = session;
      return {
        ...state,
        active: true,
        sessions: [
          session,
          ...state.sessions.filter((candidate) => candidate.id !== session.id),
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

export const adoptSession = (
  command: Extract<Command, { readonly tag: "session-adopt" }>,
): Effect.Effect<Session, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(command.scope);
    const now = new Date().toISOString();
    const nativeId = command.nativeId ?? inferredNativeId();
    const id = `${inferredSessionId(command.harness, nativeId)}-${crypto
      .randomUUID()
      .slice(0, 6)}`;
    const session: Session = {
      completion: command.completion,
      connectedAt: now,
      directory: resolved.scope,
      expectedOutput: command.expectedOutput,
      goal: command.goal,
      harness: command.harness,
      id,
      model: command.model ?? null,
      nativeId,
      nodeId: null,
      parentId: null,
      providerRef: null,
      providers: [],
      purpose: command.purpose,
      registration: "connected",
      reviewBy: command.reviewBy ?? null,
      role: "orchestrator",
      runId: null,
      status: "working",
      successCriteria: command.successCriteria,
      title: command.title,
      traceId: nativeId,
      updatedAt: now,
    };
    yield* resolved.store.update(resolved.scope, (state) => ({
      ...state,
      active: true,
      sessions: [
        session,
        ...state.sessions.map((candidate) =>
          candidate.role === "orchestrator" &&
          candidate.status !== "archived" &&
          candidate.status !== "done"
            ? { ...candidate, status: "archived" as const, updatedAt: now }
            : candidate,
        ),
      ],
      updatedAt: now,
    }));
    return yield* reconcileSession(resolved.scope, session.id).pipe(
      Effect.catch(() => Effect.succeed(session)),
    );
  });

export const archiveSession = (
  scope: string,
  selector: { readonly id?: string; readonly nativeId?: string },
): Effect.Effect<Session, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(scope);
    let result: Session | undefined;
    yield* resolved.store.update(resolved.scope, (state) => {
      const selected = sessionsByRecency(state.sessions).find((session) =>
        selector.id
          ? session.id === selector.id
          : selector.nativeId
            ? session.nativeId === selector.nativeId &&
              session.status !== "archived"
            : false,
      );
      if (!selected) return state;
      const now = new Date().toISOString();
      result = { ...selected, status: "archived", updatedAt: now };
      const sessions = state.sessions.map((session) =>
        session.id === selected.id ? (result as Session) : session,
      );
      return {
        ...state,
        active: activeSessions({ ...state, sessions }).length > 0,
        sessions,
        updatedAt: now,
      };
    });
    if (!result)
      return yield* new StateError({
        message: selector.id
          ? `unknown session: ${selector.id}`
          : `no active session has native id ${selector.nativeId ?? ""}`,
      });
    return result;
  });

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

const mergeBindings = (
  current: Session["providers"],
  discovered: Session["providers"],
): Session["providers"] => [
  ...discovered,
  ...current.filter(
    (binding) =>
      !discovered.some(
        (candidate) =>
          candidate.provider === binding.provider &&
          candidate.kind === binding.kind,
      ),
  ),
];

const genericTitle = (session: Session): boolean =>
  session.title === "Agent session" ||
  session.title === session.id ||
  session.title === session.harness;

const genericGoal = (session: Session): boolean =>
  session.goal === "Complete the assigned work" ||
  session.goal === `Run ${session.harness}`;

const sameBindings = (
  left: Session["providers"],
  right: Session["providers"],
): boolean => JSON.stringify(left) === JSON.stringify(right);

export const reconcileSession = (
  scope: string,
  id: string,
): Effect.Effect<Session, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(scope);
    const state = yield* resolved.store.read(resolved.scope);
    const session = yield* selectedSession(state, id);
    const [providers, description] = yield* Effect.all([
      discoverSessionBindings(resolved.scope, session),
      describeSession(resolved.scope, session),
    ]);
    let result = session;
    yield* resolved.store.update(resolved.scope, (current) => {
      const now = new Date().toISOString();
      let changed = false;
      const sessions = current.sessions.map((candidate) => {
        if (candidate.id !== id) return candidate;
        const next = {
          ...candidate,
          goal:
            genericGoal(candidate) && description.goal
              ? description.goal
              : candidate.goal,
          providers: mergeBindings(candidate.providers, providers),
          title:
            genericTitle(candidate) && description.title
              ? description.title
              : candidate.title,
        };
        if (
          next.goal === candidate.goal &&
          next.title === candidate.title &&
          sameBindings(next.providers, candidate.providers)
        ) {
          result = candidate;
          return candidate;
        }
        changed = true;
        result = { ...next, updatedAt: now };
        return result;
      });
      if (!changed) return current;
      return { ...current, sessions, updatedAt: now };
    });
    return result;
  });

export const reconcileWorkspace = (
  scope: string,
): Effect.Effect<WorkspaceState, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const state = yield* readWorkspace(scope);
    yield* Effect.forEach(
      activeSessions(state),
      (session) =>
        reconcileSession(state.scope, session.id).pipe(Effect.ignore),
      { concurrency: 4 },
    );
    return yield* readWorkspace(state.scope);
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
    const persisted = session.providers.some(
      (provider) =>
        provider.kind === "persistence" && provider.status === "active",
    );
    if (
      session.registration === "hook" &&
      session.status === "working" &&
      !persisted
    )
      return yield* new StateError({
        message:
          "session is active outside a persistence provider; open Activity, or archive it after the harness exits before resuming",
      });
    yield* providerOutput({
      action: "attach",
      direction: command.direction,
      scope: state.scope,
      session,
      version: "orc.provider/v1",
    });
  });

export const inspectSession = (
  command: Extract<Command, { readonly tag: "inspect" }>,
): Effect.Effect<void, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const state = yield* readWorkspace(command.scope);
    const session = yield* selectedSession(state, command.id);
    yield* providerOutput({
      action: "inspect",
      direction: command.direction,
      scope: state.scope,
      session,
      version: "orc.provider/v1",
    });
  });

export const launch = (
  command: Extract<Command, { readonly tag: "launch" }>,
): Effect.Effect<number, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const resolved = yield* resolveScope(command.scope);
    const nativeId = crypto.randomUUID();
    const sessionId = inferredSessionId(command.harness, nativeId);
    const current = currentSession(yield* resolved.store.read(resolved.scope));
    if (command.managedId) yield* resolveProviderChain("launch");
    const session = yield* registerSession({
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
      providerRef: command.managedId,
    });
    const executable = [command.harness, ...command.args];
    const environment = {
      ...process.env,
      ORC_AGENT: command.harness,
      ...(command.model ? { ORC_MODEL: command.model } : {}),
      ORC_NATIVE_SESSION_ID: nativeId,
      ...(current ? { ORC_PARENT_SESSION_ID: current.id } : {}),
      ...(command.managedId ? { ORC_PROVIDER_REF: command.managedId } : {}),
      ORC_SCOPE: resolved.scope,
      ORC_SESSION_ID: sessionId,
    };
    const execute = command.managedId
      ? invokeProvider(
          {
            action: "launch",
            command: executable,
            managedId: command.managedId,
            scope: resolved.scope,
            session,
            version: "orc.provider/v1",
          },
          environment,
          "inherit",
        ).pipe(Effect.map((response) => response.code))
      : Effect.tryPromise({
          try: async () => {
            const child = Bun.spawn(executable, {
              cwd: resolved.scope,
              env: environment,
              stderr: "inherit",
              stdin: "inherit",
              stdout: "inherit",
            });
            return await child.exited;
          },
          catch: (cause) =>
            new StateError({ message: `launch ${command.harness}`, cause }),
        });
    return yield* execute.pipe(
      Effect.tap((code) =>
        updateSessionStatus({
          id: sessionId,
          scope: resolved.scope,
          status: code === 0 ? "done" : "failed",
          tag: "session-update",
        }),
      ),
      Effect.tapError(() =>
        updateSessionStatus({
          id: sessionId,
          scope: resolved.scope,
          status: "failed",
          tag: "session-update",
        }),
      ),
    );
  });
