import { Data, Effect } from "effect";
import type {
  CompletionTarget,
  LifecycleStatus,
  SessionRole,
} from "./domain.ts";

export type Direction = "right" | "left" | "top" | "bottom";
export type CompletionShell = "bash" | "fish" | "nu" | "zsh";

interface ContractOptions {
  readonly harness: string;
  readonly model: string | undefined;
  readonly role: SessionRole;
  readonly title: string;
  readonly purpose: string;
  readonly goal: string;
  readonly expectedOutput: string;
  readonly successCriteria: ReadonlyArray<string>;
  readonly completion: CompletionTarget;
  readonly reviewBy: string | undefined;
}

export type Command =
  | { readonly tag: "tui"; readonly scope: string }
  | { readonly tag: "help" }
  | { readonly tag: "version" }
  | { readonly tag: "completion"; readonly shell: CompletionShell }
  | { readonly tag: "mcp" }
  | { readonly tag: "prompt"; readonly scope: string }
  | { readonly tag: "status"; readonly scope: string; readonly json: boolean }
  | { readonly tag: "list"; readonly scope: string; readonly json: boolean }
  | {
      readonly tag: "provider-list";
      readonly scope: string;
      readonly json: boolean;
    }
  | ({
      readonly tag: "connect";
      readonly scope: string;
      readonly id: string | undefined;
      readonly nativeId: string | undefined;
      readonly parentId: string | undefined;
      readonly providerRef: string | undefined;
    } & ContractOptions)
  | {
      readonly tag: "disconnect";
      readonly scope: string;
      readonly id: string | undefined;
    }
  | ({
      readonly tag: "session-register";
      readonly scope: string;
      readonly hookInput: boolean;
      readonly quiet: boolean;
      readonly source: "connected" | "hook" | "managed";
      readonly id: string | undefined;
      readonly nativeId: string | undefined;
      readonly parentId: string | undefined;
      readonly runId: string | undefined;
      readonly nodeId: string | undefined;
      readonly providerRef: string | undefined;
    } & ContractOptions)
  | {
      readonly tag: "session-current" | "session-list";
      readonly scope: string;
      readonly json: boolean;
    }
  | {
      readonly tag: "session-update";
      readonly scope: string;
      readonly id: string;
      readonly status: LifecycleStatus;
    }
  | {
      readonly tag: "run-create";
      readonly scope: string;
      readonly name: string;
      readonly goal: string;
      readonly expectedOutput: string;
      readonly orchestratorId: string | undefined;
      readonly harness: string | undefined;
      readonly model: string | undefined;
    }
  | {
      readonly tag: "run-agent-set";
      readonly scope: string;
      readonly id: string;
      readonly role: SessionRole;
      readonly harness: string;
      readonly model: string | undefined;
    }
  | {
      readonly tag: "run-list";
      readonly scope: string;
      readonly json: boolean;
    }
  | {
      readonly tag: "run-show";
      readonly scope: string;
      readonly id: string;
      readonly json: boolean;
    }
  | {
      readonly tag: "run-update";
      readonly scope: string;
      readonly id: string;
      readonly status: LifecycleStatus;
    }
  | ({
      readonly tag: "node-upsert";
      readonly scope: string;
      readonly runId: string;
      readonly id: string;
      readonly sessionId: string | undefined;
      readonly status: LifecycleStatus;
      readonly attempt: number;
      readonly dependsOn: ReadonlyArray<string>;
    } & ContractOptions)
  | {
      readonly tag: "node-update";
      readonly scope: string;
      readonly runId: string;
      readonly id: string;
      readonly status: LifecycleStatus;
    }
  | {
      readonly tag: "attach";
      readonly scope: string;
      readonly id: string;
      readonly direction: Direction;
    }
  | {
      readonly tag: "inspect";
      readonly scope: string;
      readonly id: string;
      readonly direction: Direction;
    }
  | {
      readonly tag: "launch";
      readonly scope: string;
      readonly harness: string;
      readonly model: string | undefined;
      readonly managedId: string | undefined;
      readonly args: ReadonlyArray<string>;
    };

export class ArgumentError extends Data.TaggedError("ArgumentError")<{
  readonly message: string;
}> {}

const roles: ReadonlyArray<SessionRole> = [
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
];

const statuses: ReadonlyArray<LifecycleStatus> = [
  "queued",
  "working",
  "waiting",
  "blocked",
  "failed",
  "done",
  "cancelled",
  "disconnected",
];

const defaultScope = (): string => process.env.ORC_SCOPE ?? process.cwd();

interface ParsedOptions {
  readonly scope: string;
  readonly json: boolean;
  readonly flags: ReadonlySet<string>;
  readonly values: Readonly<Record<string, ReadonlyArray<string>>>;
  readonly positionals: ReadonlyArray<string>;
  readonly remainder: ReadonlyArray<string>;
}

const parseOptions = (
  args: ReadonlyArray<string>,
  valueOptions: ReadonlySet<string>,
  flagOptions: ReadonlySet<string> = new Set(),
): Effect.Effect<ParsedOptions, ArgumentError> =>
  Effect.gen(function* () {
    let scope = defaultScope();
    let json = false;
    const flags = new Set<string>();
    const values: Record<string, Array<string>> = {};
    const positionals: Array<string> = [];
    let remainder: ReadonlyArray<string> = [];
    for (let index = 0; index < args.length; index++) {
      const arg = args[index] ?? "";
      if (arg === "--") {
        remainder = args.slice(index + 1);
        break;
      }
      if (arg === "--json") {
        json = true;
        continue;
      }
      if (flagOptions.has(arg)) {
        flags.add(arg);
        continue;
      }
      if (arg === "--scope" || valueOptions.has(arg)) {
        const value = args[index + 1];
        if (!value) {
          return yield* new ArgumentError({
            message: `${arg} requires a value`,
          });
        }
        if (arg === "--scope") {
          scope = value;
        } else {
          values[arg] = [...(values[arg] ?? []), value];
        }
        index++;
        continue;
      }
      if (arg.startsWith("-")) {
        return yield* new ArgumentError({ message: `unknown option: ${arg}` });
      }
      positionals.push(arg);
    }
    return { flags, json, positionals, remainder, scope, values };
  });

const one = (options: ParsedOptions, name: string): string | undefined =>
  options.values[name]?.at(-1);

const parseRole = (value: string): Effect.Effect<SessionRole, ArgumentError> =>
  roles.includes(value as SessionRole)
    ? Effect.succeed(value as SessionRole)
    : Effect.fail(
        new ArgumentError({ message: `unknown session role: ${value}` }),
      );

const parseStatus = (
  value: string | undefined,
): Effect.Effect<LifecycleStatus, ArgumentError> =>
  value && statuses.includes(value as LifecycleStatus)
    ? Effect.succeed(value as LifecycleStatus)
    : Effect.fail(
        new ArgumentError({ message: "--status requires a lifecycle status" }),
      );

const parseCompletion = (
  value: string | undefined,
): Effect.Effect<CompletionTarget, ArgumentError> =>
  value === undefined
    ? Effect.succeed("orchestrator")
    : value === "orchestrator" || value === "judge"
      ? Effect.succeed(value)
      : Effect.fail(
          new ArgumentError({
            message: "--completion must be orchestrator or judge",
          }),
        );

const parseDirection = (
  value: string | undefined,
): Effect.Effect<Direction, ArgumentError> =>
  value === undefined
    ? Effect.succeed("right")
    : value === "right" ||
        value === "left" ||
        value === "top" ||
        value === "bottom"
      ? Effect.succeed(value)
      : Effect.fail(
          new ArgumentError({
            message: "--direction must be right, left, top, or bottom",
          }),
        );

const contractOptions = new Set([
  "--harness",
  "--model",
  "--role",
  "--title",
  "--purpose",
  "--goal",
  "--expected-output",
  "--success",
  "--completion",
  "--review-by",
]);

const parseContract = (
  options: ParsedOptions,
  defaultHarness = "unknown",
): Effect.Effect<ContractOptions, ArgumentError> =>
  Effect.gen(function* () {
    const role = yield* parseRole(one(options, "--role") ?? "worker");
    const completion = yield* parseCompletion(one(options, "--completion"));
    const purpose = one(options, "--purpose") ?? "Agent session";
    return {
      completion,
      expectedOutput: one(options, "--expected-output") ?? "A verified result",
      goal: one(options, "--goal") ?? "Complete the assigned work",
      harness: one(options, "--harness") ?? defaultHarness,
      model: one(options, "--model"),
      purpose,
      reviewBy: one(options, "--review-by"),
      role,
      successCriteria: options.values["--success"] ?? [],
      title: one(options, "--title") ?? purpose,
    };
  });

const requireOne = (
  values: ReadonlyArray<string>,
  message: string,
): Effect.Effect<string, ArgumentError> =>
  values.length === 1 && values[0]
    ? Effect.succeed(values[0])
    : Effect.fail(new ArgumentError({ message }));

const parseSession = (
  rest: ReadonlyArray<string>,
): Effect.Effect<Command, ArgumentError> =>
  Effect.gen(function* () {
    const [subcommand, ...args] = rest;
    if (subcommand === "current" || subcommand === "list") {
      const options = yield* parseOptions(args, new Set());
      return {
        json: options.json,
        scope: options.scope,
        tag: `session-${subcommand}`,
      };
    }
    if (subcommand === "update") {
      const options = yield* parseOptions(args, new Set(["--status"]));
      const id = yield* requireOne(
        options.positionals,
        "session update requires one session id",
      );
      return {
        id,
        scope: options.scope,
        status: yield* parseStatus(one(options, "--status")),
        tag: "session-update",
      };
    }
    if (subcommand !== "register") {
      return yield* new ArgumentError({
        message: `unknown session command: ${subcommand ?? ""}`,
      });
    }
    const options = yield* parseOptions(
      args,
      new Set([
        ...contractOptions,
        "--id",
        "--native-id",
        "--parent",
        "--run",
        "--node",
        "--source",
        "--provider-ref",
      ]),
      new Set(["--hook-input", "--quiet"]),
    );
    const source = one(options, "--source") ?? "connected";
    if (source !== "connected" && source !== "hook" && source !== "managed") {
      return yield* new ArgumentError({
        message: "--source must be connected, hook, or managed",
      });
    }
    return {
      ...(yield* parseContract(options)),
      hookInput: options.flags.has("--hook-input"),
      id: one(options, "--id"),
      nativeId: one(options, "--native-id"),
      nodeId: one(options, "--node"),
      parentId: one(options, "--parent"),
      quiet: options.flags.has("--quiet"),
      runId: one(options, "--run"),
      scope: options.scope,
      source,
      tag: "session-register",
      providerRef: one(options, "--provider-ref"),
    };
  });

const parseRun = (
  rest: ReadonlyArray<string>,
): Effect.Effect<Command, ArgumentError> =>
  Effect.gen(function* () {
    const [subcommand, ...args] = rest;
    if (subcommand === "list") {
      const options = yield* parseOptions(args, new Set());
      return { json: options.json, scope: options.scope, tag: "run-list" };
    }
    if (subcommand === "show") {
      const options = yield* parseOptions(args, new Set());
      return {
        id: yield* requireOne(
          options.positionals,
          "run show requires one run id",
        ),
        json: options.json,
        scope: options.scope,
        tag: "run-show",
      };
    }
    if (subcommand === "update") {
      const options = yield* parseOptions(args, new Set(["--status"]));
      return {
        id: yield* requireOne(
          options.positionals,
          "run update requires one run id",
        ),
        scope: options.scope,
        status: yield* parseStatus(one(options, "--status")),
        tag: "run-update",
      };
    }
    if (subcommand === "create") {
      const options = yield* parseOptions(
        args,
        new Set([
          "--name",
          "--goal",
          "--expected-output",
          "--orchestrator",
          "--harness",
          "--model",
        ]),
      );
      const goal = one(options, "--goal") ?? "Complete the workflow";
      return {
        expectedOutput:
          one(options, "--expected-output") ?? "A verified result",
        goal,
        harness: one(options, "--harness"),
        model: one(options, "--model"),
        name: one(options, "--name") ?? goal,
        orchestratorId: one(options, "--orchestrator"),
        scope: options.scope,
        tag: "run-create",
      };
    }
    if (subcommand === "agent") {
      const options = yield* parseOptions(
        args,
        new Set(["--role", "--harness", "--model"]),
      );
      const id = yield* requireOne(
        options.positionals,
        "run agent requires one run id",
      );
      const harness = one(options, "--harness");
      if (!harness)
        return yield* new ArgumentError({
          message: "run agent requires --harness",
        });
      return {
        harness,
        id,
        model: one(options, "--model"),
        role: yield* parseRole(one(options, "--role") ?? "worker"),
        scope: options.scope,
        tag: "run-agent-set",
      };
    }
    return yield* new ArgumentError({
      message: `unknown run command: ${subcommand ?? ""}`,
    });
  });

const parseNode = (
  rest: ReadonlyArray<string>,
): Effect.Effect<Command, ArgumentError> =>
  Effect.gen(function* () {
    const [subcommand, ...args] = rest;
    if (subcommand === "update") {
      const options = yield* parseOptions(args, new Set(["--run", "--status"]));
      const id = yield* requireOne(
        options.positionals,
        "node update requires one node id",
      );
      const runId = one(options, "--run");
      if (!runId)
        return yield* new ArgumentError({
          message: "node update requires --run",
        });
      return {
        id,
        runId,
        scope: options.scope,
        status: yield* parseStatus(one(options, "--status")),
        tag: "node-update",
      };
    }
    if (subcommand !== "upsert") {
      return yield* new ArgumentError({
        message: `unknown node command: ${subcommand ?? ""}`,
      });
    }
    const options = yield* parseOptions(
      args,
      new Set([
        ...contractOptions,
        "--run",
        "--session",
        "--status",
        "--attempt",
        "--depends-on",
      ]),
    );
    const id = yield* requireOne(
      options.positionals,
      "node upsert requires one node id",
    );
    const runId = one(options, "--run");
    if (!runId)
      return yield* new ArgumentError({
        message: "node upsert requires --run",
      });
    const attempt = Number.parseInt(one(options, "--attempt") ?? "1", 10);
    if (!Number.isFinite(attempt) || attempt < 0)
      return yield* new ArgumentError({
        message: "--attempt must be a non-negative integer",
      });
    return {
      ...(yield* parseContract(options, "")),
      attempt,
      dependsOn: options.values["--depends-on"] ?? [],
      id,
      runId,
      scope: options.scope,
      sessionId: one(options, "--session"),
      status: yield* parseStatus(one(options, "--status") ?? "queued"),
      tag: "node-upsert",
    };
  });

export const parseArgs = (
  args: ReadonlyArray<string>,
): Effect.Effect<Command, ArgumentError> =>
  Effect.gen(function* () {
    const [name, ...rest] = args;
    if (!name) return { scope: defaultScope(), tag: "tui" };
    if (name === "--help" || name === "-h" || name === "help")
      return { tag: "help" };
    if (name === "--version" || name === "-v" || name === "version")
      return { tag: "version" };
    if (name === "completion") {
      const shell = yield* requireOne(rest, "completion requires one shell");
      if (
        shell !== "bash" &&
        shell !== "fish" &&
        shell !== "nu" &&
        shell !== "zsh"
      )
        return yield* new ArgumentError({
          message: `unsupported completion shell: ${shell}`,
        });
      return { shell, tag: "completion" };
    }
    if (name === "mcp") return { tag: "mcp" };
    if (name === "providers") {
      const options = yield* parseOptions(rest, new Set());
      return {
        json: options.json,
        scope: options.scope,
        tag: "provider-list",
      };
    }
    if (name === "session") return yield* parseSession(rest);
    if (name === "run") return yield* parseRun(rest);
    if (name === "node") return yield* parseNode(rest);
    if (
      name === "prompt" ||
      name === "tui" ||
      name === "status" ||
      name === "list"
    ) {
      const options = yield* parseOptions(rest, new Set());
      return name === "status" || name === "list"
        ? { json: options.json, scope: options.scope, tag: name }
        : { scope: options.scope, tag: name };
    }
    if (name === "connect") {
      const options = yield* parseOptions(
        rest,
        new Set([
          ...contractOptions,
          "--id",
          "--native-id",
          "--parent",
          "--provider-ref",
        ]),
      );
      return {
        ...(yield* parseContract(options)),
        id: one(options, "--id"),
        nativeId: one(options, "--native-id"),
        parentId: one(options, "--parent"),
        scope: options.scope,
        tag: "connect",
        providerRef: one(options, "--provider-ref"),
      };
    }
    if (name === "disconnect") {
      const options = yield* parseOptions(rest, new Set());
      if (options.positionals.length > 1)
        return yield* new ArgumentError({
          message: "disconnect accepts one session id",
        });
      return {
        id: options.positionals[0],
        scope: options.scope,
        tag: "disconnect",
      };
    }
    if (name === "attach" || name === "inspect") {
      const options = yield* parseOptions(rest, new Set(["--direction"]));
      return {
        direction: yield* parseDirection(one(options, "--direction")),
        id: yield* requireOne(
          options.positionals,
          `${name} requires one session id`,
        ),
        scope: options.scope,
        tag: name,
      };
    }
    if (name === "launch") {
      const options = yield* parseOptions(
        rest,
        new Set(["--managed", "--model"]),
      );
      const harness = yield* requireOne(
        options.positionals,
        "launch requires one harness command",
      );
      return {
        args: options.remainder,
        harness,
        model: one(options, "--model"),
        scope: options.scope,
        tag: "launch",
        managedId: one(options, "--managed"),
      };
    }
    return yield* new ArgumentError({ message: `unknown command: ${name}` });
  });
