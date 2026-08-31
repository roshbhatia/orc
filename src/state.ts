import { mkdir, readFile, realpath, rename, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { Context, Data, Effect, Layer, Schema } from "effect";
import {
  emptyWorkspace,
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
  if (!home) {
    return join(process.cwd(), ".orc-state");
  }
  return join(home, ".local", "state", "orc");
};

const scopeKey = (scope: string): string => {
  const hasher = new Bun.CryptoHasher("sha256");
  hasher.update(scope);
  return hasher.digest("hex").slice(0, 20);
};

export const statePath = (scope: string): string =>
  join(stateRoot(), `${scopeKey(scope)}.json`);

const decodeState = (
  value: unknown,
): Effect.Effect<WorkspaceState, StateError> =>
  Schema.decodeUnknownEffect(WorkspaceStateSchema)(value).pipe(
    Effect.mapError(
      (cause) =>
        new StateError({
          message: "state file does not match orc.state/v1",
          cause,
        }),
    ),
  );

const resolveScope = (directory: string): Effect.Effect<string, StateError> =>
  Effect.tryPromise({
    try: () => realpath(directory),
    catch: (cause) =>
      new StateError({ message: `resolve scope: ${directory}`, cause }),
  });

const isMissingFile = (cause: unknown): boolean =>
  cause instanceof Error && "code" in cause && cause.code === "ENOENT";

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
      if (isMissingFile(cause)) {
        return Effect.succeed(emptyWorkspace(scope));
      }
      return Effect.fail(
        cause instanceof StateError
          ? cause
          : new StateError({ message: "read workspace state", cause }),
      );
    }),
  );

const write = (state: WorkspaceState): Effect.Effect<void, StateError> =>
  Effect.tryPromise({
    try: async () => {
      const target = statePath(state.scope);
      const temporary = `${target}.${process.pid}.tmp`;
      await mkdir(dirname(target), { recursive: true, mode: 0o700 });
      await writeFile(temporary, `${JSON.stringify(state, null, 2)}\n`, {
        mode: 0o600,
      });
      await rename(temporary, target);
    },
    catch: (cause) =>
      new StateError({ message: "write workspace state", cause }),
  });

export const StateStoreLive = Layer.succeed(StateStore, {
  read,
  resolveScope,
  write,
});
