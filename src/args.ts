import { Data, Effect } from "effect";
import type { SessionRole } from "./domain.ts";

export type Direction = "right" | "left" | "top" | "bottom";

export type Command =
  | { readonly tag: "tui"; readonly scope: string }
  | { readonly tag: "help" }
  | { readonly tag: "version" }
  | { readonly tag: "status"; readonly scope: string; readonly json: boolean }
  | { readonly tag: "list"; readonly scope: string; readonly json: boolean }
  | {
      readonly tag: "connect";
      readonly scope: string;
      readonly id: string | undefined;
      readonly nativeId: string | undefined;
      readonly harness: string;
      readonly role: SessionRole;
      readonly purpose: string;
      readonly goal: string;
      readonly expectedOutput: string;
      readonly completion: "orchestrator" | "judge";
      readonly parentId: string | undefined;
      readonly zmxSession: string | undefined;
    }
  | {
      readonly tag: "disconnect";
      readonly scope: string;
      readonly id: string | undefined;
    }
  | {
      readonly tag: "attach";
      readonly scope: string;
      readonly id: string;
      readonly direction: Direction;
    }
  | {
      readonly tag: "traces";
      readonly scope: string;
      readonly id: string;
      readonly direction: Direction;
    };

export class ArgumentError extends Data.TaggedError("ArgumentError")<{
  readonly message: string;
}> {}

const roles: ReadonlyArray<SessionRole> = [
  "orchestrator",
  "planner",
  "researcher",
  "implementer",
  "judge",
  "worker",
];

const valueAfter = (
  args: ReadonlyArray<string>,
  index: number,
  name: string,
): Effect.Effect<string, ArgumentError> => {
  const value = args[index + 1];
  return value
    ? Effect.succeed(value)
    : Effect.fail(new ArgumentError({ message: `${name} requires a value` }));
};

const parseDirection = (
  value: string,
): Effect.Effect<Direction, ArgumentError> =>
  value === "right" || value === "left" || value === "top" || value === "bottom"
    ? Effect.succeed(value)
    : Effect.fail(
        new ArgumentError({
          message: "--direction must be right, left, top, or bottom",
        }),
      );

const parseRole = (value: string): Effect.Effect<SessionRole, ArgumentError> =>
  roles.includes(value as SessionRole)
    ? Effect.succeed(value as SessionRole)
    : Effect.fail(
        new ArgumentError({ message: `unknown session role: ${value}` }),
      );

interface CommonOptions {
  readonly scope: string;
  readonly json: boolean;
  readonly values: Readonly<Record<string, string>>;
  readonly positionals: ReadonlyArray<string>;
}

const parseOptions = (
  args: ReadonlyArray<string>,
  valueOptions: ReadonlySet<string>,
): Effect.Effect<CommonOptions, ArgumentError> =>
  Effect.gen(function* () {
    let scope = process.cwd();
    let json = false;
    const values: Record<string, string> = {};
    const positionals: Array<string> = [];
    for (let index = 0; index < args.length; index++) {
      const arg = args[index] ?? "";
      if (arg === "--json") {
        json = true;
      } else if (arg === "--scope") {
        scope = yield* valueAfter(args, index, arg);
        index++;
      } else if (valueOptions.has(arg)) {
        values[arg] = yield* valueAfter(args, index, arg);
        index++;
      } else if (arg.startsWith("-")) {
        return yield* new ArgumentError({ message: `unknown option: ${arg}` });
      } else {
        positionals.push(arg);
      }
    }
    return { json, positionals, scope, values };
  });

export const parseArgs = (
  args: ReadonlyArray<string>,
): Effect.Effect<Command, ArgumentError> =>
  Effect.gen(function* () {
    const [name, ...rest] = args;
    if (!name) {
      return { scope: process.cwd(), tag: "tui" };
    }
    if (name === "--help" || name === "-h" || name === "help") {
      return { tag: "help" };
    }
    if (name === "--version" || name === "-v" || name === "version") {
      return { tag: "version" };
    }
    if (name === "status" || name === "list") {
      const options = yield* parseOptions(rest, new Set());
      return {
        json: options.json,
        scope: options.scope,
        tag: name,
      };
    }
    if (name === "connect") {
      const options = yield* parseOptions(
        rest,
        new Set([
          "--id",
          "--native-id",
          "--harness",
          "--role",
          "--purpose",
          "--goal",
          "--expected-output",
          "--completion",
          "--parent",
          "--zmx",
        ]),
      );
      const role = yield* parseRole(options.values["--role"] ?? "worker");
      const completion = options.values["--completion"] ?? "orchestrator";
      if (completion !== "orchestrator" && completion !== "judge") {
        return yield* new ArgumentError({
          message: "--completion must be orchestrator or judge",
        });
      }
      return {
        completion,
        expectedOutput:
          options.values["--expected-output"] ?? "A verified result",
        goal: options.values["--goal"] ?? "Complete the assigned work",
        harness: options.values["--harness"] ?? "unknown",
        id: options.values["--id"],
        nativeId: options.values["--native-id"],
        parentId: options.values["--parent"],
        purpose: options.values["--purpose"] ?? "Agent session",
        role,
        scope: options.scope,
        tag: "connect",
        zmxSession: options.values["--zmx"],
      };
    }
    if (name === "disconnect") {
      const options = yield* parseOptions(rest, new Set());
      if (options.positionals.length > 1) {
        return yield* new ArgumentError({
          message: "disconnect accepts one session id",
        });
      }
      return {
        id: options.positionals[0],
        scope: options.scope,
        tag: "disconnect",
      };
    }
    if (name === "attach" || name === "traces") {
      const options = yield* parseOptions(rest, new Set(["--direction"]));
      const id = options.positionals[0];
      if (!id || options.positionals.length !== 1) {
        return yield* new ArgumentError({
          message: `${name} requires one session id`,
        });
      }
      const direction = yield* parseDirection(
        options.values["--direction"] ?? "right",
      );
      return { direction, id, scope: options.scope, tag: name };
    }
    if (name === "tui") {
      const options = yield* parseOptions(rest, new Set());
      return { scope: options.scope, tag: "tui" };
    }
    return yield* new ArgumentError({ message: `unknown command: ${name}` });
  });
