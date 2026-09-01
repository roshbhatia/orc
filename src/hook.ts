import { Effect } from "effect";
import type { Command } from "./args.ts";
import { readWorkspace, registerSession } from "./control.ts";
import type { Session } from "./domain.ts";
import { StateError, type StateStoreService } from "./state.ts";

interface HookContext {
  readonly directory: string | undefined;
  readonly nativeId: string | undefined;
  readonly traceId: string | undefined;
  readonly title: string | undefined;
  readonly goal: string | undefined;
}

const record = (value: unknown): Readonly<Record<string, unknown>> | null =>
  typeof value === "object" && value !== null
    ? (value as Readonly<Record<string, unknown>>)
    : null;

const firstString = (
  value: Readonly<Record<string, unknown>>,
  names: ReadonlyArray<string>,
): string | undefined => {
  for (const name of names) {
    const candidate = value[name];
    if (typeof candidate === "string" && candidate.length > 0) return candidate;
  }
  return undefined;
};

export const parseHookContext = (value: unknown): HookContext => {
  const source = record(value) ?? {};
  return {
    directory: firstString(source, ["cwd", "directory", "workspace"]),
    goal: firstString(source, ["goal", "prompt", "description"]),
    nativeId: firstString(source, [
      "session_id",
      "thread_id",
      "sessionId",
      "threadId",
    ]),
    title: firstString(source, ["title", "summary", "name"]),
    traceId: firstString(source, ["trace_id", "thread_id", "session_id"]),
  };
};

const readHookInput = (): Effect.Effect<unknown, StateError> =>
  Effect.tryPromise({
    try: async () => {
      const text = await Bun.stdin.text();
      return text.trim().length > 0 ? (JSON.parse(text) as unknown) : {};
    },
    catch: (cause) =>
      new StateError({ message: "read session hook input", cause }),
  });

export const registerFromHook = (
  command: Extract<Command, { readonly tag: "session-register" }>,
): Effect.Effect<Session | null, StateError, StateStoreService> =>
  Effect.gen(function* () {
    const context: HookContext = command.hookInput
      ? parseHookContext(yield* readHookInput())
      : {
          directory: undefined,
          goal: undefined,
          nativeId: undefined,
          title: undefined,
          traceId: undefined,
        };
    const directory = context.directory ?? command.scope;
    const state = yield* readWorkspace(directory);
    if (command.hookInput && !process.env.ORC_SCOPE && !state.active)
      return null;
    const role =
      command.hookInput && !process.env.ORC_PARENT_SESSION_ID
        ? "orchestrator"
        : command.role;
    return yield* registerSession(
      {
        ...command,
        goal: context.goal ?? command.goal,
        parentId: command.parentId ?? process.env.ORC_PARENT_SESSION_ID,
        role,
        title: context.title ?? command.title,
        providerRef: command.providerRef ?? process.env.ORC_PROVIDER_REF,
      },
      {
        directory,
        ...(context.nativeId ? { nativeId: context.nativeId } : {}),
        ...(context.traceId ? { traceId: context.traceId } : {}),
      },
    );
  });
