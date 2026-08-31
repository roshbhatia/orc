import { Effect } from "effect";
import type { Command, Direction } from "./args.ts";
import { activeSessions, type Session, type WorkspaceState } from "./domain.ts";
import { StateError, StateStore, type StateStoreService } from "./state.ts";

const inferredNativeId = (): string =>
  process.env.ORC_NATIVE_SESSION_ID ??
  process.env.CODEX_THREAD_ID ??
  process.env.CLAUDE_SESSION_ID ??
  process.env.OPENCODE_SESSION_ID ??
  `process-${process.ppid}`;

const inferredSessionId = (harness: string, nativeId: string): string => {
  const hasher = new Bun.CryptoHasher("sha256");
  hasher.update(`${harness}:\0:${nativeId}`);
  return `${harness}-${hasher.digest("hex").slice(0, 12)}`;
};

const resolveState = (scope: string) =>
  Effect.gen(function* () {
    const store = yield* StateStore;
    const resolved = yield* store.resolveScope(scope);
    const state = yield* store.read(resolved);
    return { state, store };
  });

const chooseParent = (
  state: WorkspaceState,
  role: Session["role"],
  requested: string | undefined,
): string | null => {
  if (requested) {
    return requested;
  }
  if (role === "orchestrator") {
    return null;
  }
  return (
    activeSessions(state).find((session) => session.role === "orchestrator")
      ?.id ?? null
  );
};

export const connect = (
  command: Extract<Command, { readonly tag: "connect" }>,
): Effect.Effect<Session, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const { state, store } = yield* resolveState(command.scope);
    const now = new Date().toISOString();
    const nativeId = command.nativeId ?? inferredNativeId();
    const id =
      command.id ??
      process.env.ORC_SESSION_ID ??
      inferredSessionId(command.harness, nativeId);
    const current = state.sessions.find((session) => session.id === id);
    const session: Session = {
      completion: command.completion,
      connectedAt: current?.connectedAt ?? now,
      expectedOutput: command.expectedOutput,
      goal: command.goal,
      harness: command.harness,
      id,
      nativeId,
      parentId: chooseParent(state, command.role, command.parentId),
      purpose: command.purpose,
      role: command.role,
      status: "working",
      updatedAt: now,
      zmxSession: command.zmxSession ?? null,
    };
    const next: WorkspaceState = {
      ...state,
      active: true,
      sessions: [
        session,
        ...state.sessions.filter((candidate) => candidate.id !== id),
      ],
      updatedAt: now,
    };
    yield* store.write(next);
    return session;
  });

export const disconnect = (
  command: Extract<Command, { readonly tag: "disconnect" }>,
): Effect.Effect<Session, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const { state, store } = yield* resolveState(command.scope);
    const id = command.id ?? process.env.ORC_SESSION_ID;
    if (!id) {
      return yield* new StateError({
        message: "disconnect requires a session id or ORC_SESSION_ID",
      });
    }
    const current = state.sessions.find((session) => session.id === id);
    if (!current) {
      return yield* new StateError({ message: `unknown session: ${id}` });
    }
    const now = new Date().toISOString();
    const session: Session = {
      ...current,
      status: "disconnected",
      updatedAt: now,
    };
    const sessions = state.sessions.map((candidate) =>
      candidate.id === id ? session : candidate,
    );
    const active = activeSessions({ ...state, sessions }).length > 0;
    yield* store.write({ ...state, active, sessions, updatedAt: now });
    return session;
  });

export const readWorkspace = (
  scope: string,
): Effect.Effect<WorkspaceState, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const { state } = yield* resolveState(scope);
    return state;
  });

const splitFlag = (direction: Direction): string => `--${direction}`;

const spawnSplit = (
  scope: string,
  direction: Direction,
  command: ReadonlyArray<string>,
): Effect.Effect<void, StateError> =>
  Effect.tryPromise({
    try: async () => {
      const process = Bun.spawn(
        [
          "wezterm",
          "cli",
          "split-pane",
          "--no-auto-start",
          splitFlag(direction),
          "--cwd",
          scope,
          "--",
          ...command,
        ],
        { stderr: "inherit", stdout: "inherit" },
      );
      const code = await process.exited;
      if (code !== 0) {
        throw new Error(`wezterm split exited with code ${code}`);
      }
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
    if (!session.zmxSession) {
      return yield* new StateError({
        message: `session ${session.id} has no ZMX attachment`,
      });
    }
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
      session.nativeId,
    ]);
  });
