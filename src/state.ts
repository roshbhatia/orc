import {
  mkdir,
  readFile,
  realpath,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import { dirname, join } from "node:path";
import { Context, Data, Effect, Layer, Schema } from "effect";
import {
  emptyWorkspace,
  inferredSessionId,
  type Session,
  type WorkspaceState,
  WorkspaceStateSchema,
} from "./domain.ts";

export class StateError extends Data.TaggedError("StateError")<{
  readonly message: string;
  readonly cause?: unknown;
}> {}

export interface StateStoreService {
  readonly resolveScope: (
    directory: string,
  ) => Effect.Effect<string, StateError>;
  readonly read: (scope: string) => Effect.Effect<WorkspaceState, StateError>;
  readonly write: (state: WorkspaceState) => Effect.Effect<void, StateError>;
  readonly update: (
    scope: string,
    transform: (state: WorkspaceState) => WorkspaceState,
  ) => Effect.Effect<WorkspaceState, StateError>;
}

export const StateStore = Context.Service<StateStoreService>(
  "@roshbhatia/orc/StateStore",
);

const stateRoot = (): string => {
  const configured = process.env.XDG_STATE_HOME;
  if (configured) {
    return join(configured, "orc");
  }
  const home = process.env.HOME;
  return home
    ? join(home, ".local", "state", "orc")
    : join(process.cwd(), ".orc-state");
};

const scopeKey = (scope: string): string => {
  const hasher = new Bun.CryptoHasher("sha256");
  hasher.update(scope);
  return hasher.digest("hex").slice(0, 20);
};

export const statePath = (scope: string): string =>
  join(stateRoot(), `${scopeKey(scope)}.json`);

const record = (value: unknown): Readonly<Record<string, unknown>> | null =>
  typeof value === "object" && value !== null
    ? (value as Readonly<Record<string, unknown>>)
    : null;

const stringValue = (
  value: Readonly<Record<string, unknown>>,
  name: string,
  fallback: string,
): string => (typeof value[name] === "string" ? value[name] : fallback);

const nullableString = (
  value: Readonly<Record<string, unknown>>,
  name: string,
): string | null => (typeof value[name] === "string" ? value[name] : null);

const migrateSession = (value: unknown, now: string): Session | null => {
  const item = record(value);
  if (!item) {
    return null;
  }
  const id = stringValue(item, "id", "");
  if (!id) {
    return null;
  }
  const nativeId = stringValue(item, "nativeId", id);
  return {
    completion: "orchestrator",
    connectedAt: stringValue(item, "connectedAt", now),
    directory: stringValue(item, "directory", ""),
    expectedOutput: stringValue(item, "expectedOutput", "A verified result"),
    goal: stringValue(item, "goal", "Complete the assigned work"),
    harness: stringValue(item, "harness", "unknown"),
    model: nullableString(item, "model"),
    id,
    nativeId,
    nodeId: null,
    parentId: nullableString(item, "parentId"),
    purpose: stringValue(item, "purpose", "Agent session"),
    providers: [],
    registration: "connected",
    reviewBy: null,
    role: item.role === "orchestrator" ? "orchestrator" : "worker",
    runId: null,
    status: item.status === "disconnected" ? "disconnected" : "working",
    successCriteria: [],
    title: stringValue(item, "purpose", id),
    traceId: nativeId,
    updatedAt: stringValue(item, "updatedAt", now),
    providerRef:
      nullableString(item, "providerRef") ?? nullableString(item, "zmxSession"),
  };
};

const migrateV1 = (value: unknown): unknown | null => {
  const source = record(value);
  if (source?.schemaVersion !== "orc.state/v1") {
    return null;
  }
  const scope = stringValue(source, "scope", "");
  const now = stringValue(source, "updatedAt", new Date().toISOString());
  const sessions = Array.isArray(source.sessions)
    ? source.sessions.flatMap((item) => {
        const session = migrateSession(item, now);
        return session ? [session] : [];
      })
    : [];
  return {
    active: source.active === true,
    runs: [],
    schemaVersion: "orc.state/v2",
    scope,
    sessions,
    updatedAt: now,
  };
};

const normalizeV2 = (value: unknown): unknown => {
  const source = record(value);
  if (source?.schemaVersion !== "orc.state/v2") return value;
  const sessions = Array.isArray(source.sessions)
    ? source.sessions.map((value) => {
        const session = record(value);
        const harness = session
          ? stringValue(session, "harness", "unknown")
          : "unknown";
        const nativeId = session ? stringValue(session, "nativeId", "") : "";
        const id = session ? stringValue(session, "id", "").trim() : "";
        return session
          ? {
              model: null,
              providerRef: nullableString(session, "zmxSession"),
              ...session,
              id: id || inferredSessionId(harness, nativeId),
            }
          : value;
      })
    : [];
  const runs = Array.isArray(source.runs)
    ? source.runs.map((value) => {
        const run = record(value);
        if (!run) return value;
        const nodes = Array.isArray(run.nodes)
          ? run.nodes.map((value) => {
              const node = record(value);
              return node ? { model: null, ...node } : value;
            })
          : [];
        return { agents: [], ...run, nodes };
      })
    : [];
  return { ...source, runs, sessions };
};

const migrateV2 = (value: unknown): unknown => {
  const normalized = record(normalizeV2(value));
  if (normalized?.schemaVersion !== "orc.state/v2") return value;
  const sessions = Array.isArray(normalized.sessions)
    ? normalized.sessions.map((value) => {
        const session = record(value);
        if (!session) return value;
        const providerRef = nullableString(session, "providerRef");
        return {
          ...session,
          providers: providerRef
            ? [
                {
                  kind: "persistence",
                  label: providerRef,
                  provider: "legacy",
                  ref: providerRef,
                  status: "active",
                },
              ]
            : [],
        };
      })
    : [];
  return { ...normalized, schemaVersion: "orc.state/v3", sessions };
};

const normalizeV3 = (value: unknown): unknown => {
  const source = record(value);
  if (source?.schemaVersion !== "orc.state/v3") return value;
  const sessions = Array.isArray(source.sessions)
    ? source.sessions.map((value) => {
        const session = record(value);
        return session ? { providers: [], ...session } : value;
      })
    : [];
  return { ...source, sessions };
};

const decodeState = (
  value: unknown,
): Effect.Effect<WorkspaceState, StateError> => {
  const migrated = migrateV2(migrateV1(value) ?? value);
  return Schema.decodeUnknownEffect(WorkspaceStateSchema)(
    normalizeV3(migrated),
  ).pipe(
    Effect.mapError(
      (cause) =>
        new StateError({
          message: "state file does not match orc.state/v3",
          cause,
        }),
    ),
  );
};

const resolveScope = (directory: string): Effect.Effect<string, StateError> =>
  Effect.tryPromise({
    try: () => realpath(directory),
    catch: (cause) =>
      new StateError({ message: `resolve scope: ${directory}`, cause }),
  });

const isCode = (cause: unknown, code: string): boolean =>
  cause instanceof Error && "code" in cause && cause.code === code;

const read = (scope: string): Effect.Effect<WorkspaceState, StateError> =>
  Effect.tryPromise({
    try: () => readFile(statePath(scope), "utf8"),
    catch: (cause) => cause,
  }).pipe(
    Effect.flatMap((contents) =>
      Effect.try({
        try: () => JSON.parse(contents) as unknown,
        catch: (cause) =>
          new StateError({ message: "parse workspace state", cause }),
      }),
    ),
    Effect.flatMap(decodeState),
    Effect.catch((cause) => {
      if (isCode(cause, "ENOENT")) {
        return Effect.succeed(emptyWorkspace(scope));
      }
      return Effect.fail(
        cause instanceof StateError
          ? cause
          : new StateError({ message: "read workspace state", cause }),
      );
    }),
  );

const writeFileAtomic = async (state: WorkspaceState): Promise<void> => {
  const target = statePath(state.scope);
  const temporary = `${target}.${process.pid}.${crypto.randomUUID()}.tmp`;
  await mkdir(dirname(target), { recursive: true, mode: 0o700 });
  await writeFile(temporary, `${JSON.stringify(state, null, 2)}\n`, {
    mode: 0o600,
  });
  await rename(temporary, target);
};

const write = (state: WorkspaceState): Effect.Effect<void, StateError> =>
  Effect.tryPromise({
    try: () => writeFileAtomic(state),
    catch: (cause) =>
      new StateError({ message: "write workspace state", cause }),
  });

const acquireLock = async (target: string): Promise<void> => {
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      await mkdir(target, { mode: 0o700 });
      return;
    } catch (cause) {
      if (!isCode(cause, "EEXIST")) {
        throw cause;
      }
      await Bun.sleep(10);
    }
  }
  throw new Error(`timed out waiting for state lock: ${target}`);
};

const update = (
  scope: string,
  transform: (state: WorkspaceState) => WorkspaceState,
): Effect.Effect<WorkspaceState, StateError> =>
  Effect.tryPromise({
    try: async () => {
      const lock = `${statePath(scope)}.lock`;
      await mkdir(dirname(lock), { recursive: true, mode: 0o700 });
      await acquireLock(lock);
      try {
        const current = await Effect.runPromise(read(scope));
        const next = transform(current);
        await writeFileAtomic(next);
        return next;
      } finally {
        await rm(lock, { force: true, recursive: true });
      }
    },
    catch: (cause) =>
      cause instanceof StateError
        ? cause
        : new StateError({ message: "update workspace state", cause }),
  });

export const StateStoreLive = Layer.succeed(StateStore, {
  read,
  resolveScope,
  update,
  write,
});
