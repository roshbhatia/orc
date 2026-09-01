import type { Dirent } from "node:fs";
import { readdir } from "node:fs/promises";
import { homedir } from "node:os";
import { extname, isAbsolute, join, sep } from "node:path";
import { Effect } from "effect";
import type { Direction } from "./args.ts";
import type { Session } from "./domain.ts";
import { StateError } from "./state.ts";

export type ProviderAction = "attach" | "inspect" | "changes" | "launch";

export type ProviderCapability =
  | "changes.inspect"
  | "session.attach"
  | "session.inspect"
  | "session.launch"
  | "terminal.open";

interface ProviderRequestBase {
  readonly version: "orc.provider/v1";
  readonly scope: string;
}

export type ProviderRequest =
  | (ProviderRequestBase & {
      readonly action: "attach" | "inspect";
      readonly direction: Direction;
      readonly session: Session;
    })
  | (ProviderRequestBase & {
      readonly action: "changes";
    })
  | (ProviderRequestBase & {
      readonly action: "launch";
      readonly command: ReadonlyArray<string>;
      readonly managedId: string;
      readonly session: Session;
    });

export interface CommandPlan {
  readonly version: "orc.provider/v1";
  readonly command: ReadonlyArray<string>;
  readonly cwd?: string;
  readonly environment?: Readonly<Record<string, string>>;
}

export interface ProviderResponse {
  readonly code: number;
  readonly stderr: string;
  readonly stdout: string;
}

export interface ResolvedProvider {
  readonly capability: ProviderCapability;
  readonly command: string;
  readonly name: string;
  readonly priority: number;
}

interface ProviderManifest {
  readonly capabilities: ReadonlyArray<ProviderCapability>;
  readonly command: string;
  readonly name: string;
  readonly priority: number;
  readonly version: "orc.provider/v1";
}

type ProviderStageRequest = ProviderRequest & {
  readonly capability: ProviderCapability;
  readonly plan: CommandPlan | null;
};

const capabilities = [
  "changes.inspect",
  "session.attach",
  "session.inspect",
  "session.launch",
  "terminal.open",
] as const;

const actionCapabilities: Readonly<
  Record<ProviderAction, ReadonlyArray<ProviderCapability>>
> = {
  attach: ["session.attach", "terminal.open"],
  changes: ["changes.inspect"],
  inspect: ["session.inspect", "terminal.open"],
  launch: ["session.launch"],
};

const record = (value: unknown): Readonly<Record<string, unknown>> | null =>
  typeof value === "object" && value !== null
    ? (value as Readonly<Record<string, unknown>>)
    : null;

const isCapability = (value: string): value is ProviderCapability =>
  capabilities.some((capability) => capability === value);

const providerDirectory = (
  environment: Readonly<Record<string, string | undefined>>,
): string =>
  environment.ORC_PROVIDER_DIR ??
  join(
    environment.XDG_CONFIG_HOME ??
      join(environment.HOME ?? homedir(), ".config"),
    "orc",
    "providers",
  );

const resolveCommand = (
  command: string,
  environment: Readonly<Record<string, string | undefined>>,
): string | null => {
  if (isAbsolute(command) || command.includes(sep)) return command;
  return Bun.which(
    command,
    environment.PATH ? { PATH: environment.PATH } : undefined,
  );
};

const parseManifest = (
  value: unknown,
  path: string,
  environment: Readonly<Record<string, string | undefined>>,
): ProviderManifest => {
  const parsed = record(value);
  if (!parsed) throw new Error(`${path}: manifest must be an object`);
  if (parsed.version !== "orc.provider/v1")
    throw new Error(`${path}: version must be orc.provider/v1`);
  if (
    typeof parsed.name !== "string" ||
    !/^[a-z0-9][a-z0-9._-]*$/.test(parsed.name)
  )
    throw new Error(
      `${path}: name must use lowercase letters, numbers, dots, underscores, or dashes`,
    );
  if (typeof parsed.command !== "string" || parsed.command.trim().length === 0)
    throw new Error(`${path}: command must be a non-empty string`);
  const command = resolveCommand(parsed.command.trim(), environment);
  if (!command)
    throw new Error(`${path}: command ${parsed.command} was not found on PATH`);
  if (!Array.isArray(parsed.capabilities) || parsed.capabilities.length === 0)
    throw new Error(`${path}: capabilities must be a non-empty array`);
  const selected: ProviderCapability[] = [];
  for (const value of parsed.capabilities) {
    if (typeof value !== "string" || !isCapability(value))
      throw new Error(`${path}: unsupported capability ${String(value)}`);
    if (!selected.includes(value)) selected.push(value);
  }
  const priority = parsed.priority ?? 0;
  if (typeof priority !== "number" || !Number.isSafeInteger(priority))
    throw new Error(`${path}: priority must be an integer`);
  return {
    capabilities: selected,
    command,
    name: parsed.name,
    priority,
    version: "orc.provider/v1",
  };
};

const discoverProviders = (
  environment: Readonly<Record<string, string | undefined>>,
): Effect.Effect<ReadonlyArray<ProviderManifest>, StateError> =>
  Effect.tryPromise({
    try: async () => {
      const directory = providerDirectory(environment);
      let entries: Dirent[];
      try {
        entries = await readdir(directory, { withFileTypes: true });
      } catch (cause) {
        if (record(cause)?.code === "ENOENT") return [];
        throw cause;
      }
      const manifests: ProviderManifest[] = [];
      for (const entry of entries.sort((left, right) =>
        left.name.localeCompare(right.name),
      )) {
        if (!entry.isFile() || extname(entry.name) !== ".json") continue;
        const path = join(directory, entry.name);
        manifests.push(
          parseManifest(await Bun.file(path).json(), path, environment),
        );
      }
      return manifests;
    },
    catch: (cause) =>
      new StateError({
        message: `read provider manifests from ${providerDirectory(environment)}`,
        cause,
      }),
  });

export const resolveProviderChain = (
  action: ProviderAction,
  environment: Readonly<Record<string, string | undefined>> = process.env,
): Effect.Effect<ReadonlyArray<ResolvedProvider>, StateError> =>
  Effect.gen(function* () {
    const manifests = yield* discoverProviders(environment);
    const chain: ResolvedProvider[] = [];
    for (const capability of actionCapabilities[action]) {
      const candidates = manifests
        .filter((manifest) => manifest.capabilities.includes(capability))
        .sort(
          (left, right) =>
            right.priority - left.priority ||
            left.name.localeCompare(right.name),
        );
      const selected = candidates[0];
      if (!selected)
        return yield* new StateError({
          message: `no provider advertises capability ${capability}`,
        });
      const ambiguous = candidates.filter(
        (candidate) => candidate.priority === selected.priority,
      );
      if (ambiguous.length > 1)
        return yield* new StateError({
          message: `ambiguous providers for ${capability}: ${ambiguous
            .map((candidate) => candidate.name)
            .join(", ")}`,
        });
      chain.push({
        capability,
        command: selected.command,
        name: selected.name,
        priority: selected.priority,
      });
    }
    return chain;
  });

const parseCommandPlan = (value: unknown, provider: string): CommandPlan => {
  const parsed = record(value);
  if (!parsed) throw new Error(`${provider}: response must be an object`);
  if (parsed.version !== "orc.provider/v1")
    throw new Error(`${provider}: response version must be orc.provider/v1`);
  if (
    !Array.isArray(parsed.command) ||
    parsed.command.length === 0 ||
    parsed.command.some((part) => typeof part !== "string" || part.length === 0)
  )
    throw new Error(
      `${provider}: response command must contain non-empty strings`,
    );
  if (
    parsed.cwd !== undefined &&
    (typeof parsed.cwd !== "string" || parsed.cwd.length === 0)
  )
    throw new Error(`${provider}: response cwd must be a non-empty string`);
  const environment = record(parsed.environment);
  if (parsed.environment !== undefined && !environment)
    throw new Error(`${provider}: response environment must be an object`);
  const selectedEnvironment: Record<string, string> = {};
  for (const [name, value] of Object.entries(environment ?? {})) {
    if (typeof value !== "string")
      throw new Error(`${provider}: environment.${name} must be a string`);
    selectedEnvironment[name] = value;
  }
  return {
    command: parsed.command as ReadonlyArray<string>,
    ...(typeof parsed.cwd === "string" ? { cwd: parsed.cwd } : {}),
    ...(Object.keys(selectedEnvironment).length > 0
      ? { environment: selectedEnvironment }
      : {}),
    version: "orc.provider/v1",
  };
};

const runProviderStage = (
  provider: ResolvedProvider,
  request: ProviderRequest,
  plan: CommandPlan | null,
  environment: Readonly<Record<string, string | undefined>>,
): Effect.Effect<CommandPlan, StateError> =>
  Effect.tryPromise({
    try: async () => {
      const stageRequest: ProviderStageRequest = {
        ...request,
        capability: provider.capability,
        plan,
      };
      const child = Bun.spawn([provider.command], {
        cwd: request.scope,
        env: environment,
        stderr: "pipe",
        stdin: "pipe",
        stdout: "pipe",
      });
      child.stdin.write(JSON.stringify(stageRequest));
      child.stdin.end();
      const [stdout, stderr, code] = await Promise.all([
        new Response(child.stdout).text(),
        new Response(child.stderr).text(),
        child.exited,
      ]);
      if (code !== 0)
        throw new Error(
          stderr.trim() || `${provider.name} exited with code ${code}`,
        );
      return parseCommandPlan(JSON.parse(stdout), provider.name);
    },
    catch: (cause) =>
      new StateError({
        message: `run ${provider.name} for ${provider.capability}`,
        cause,
      }),
  });

export const resolveCommandPlan = (
  request: ProviderRequest,
  environment: Readonly<Record<string, string | undefined>> = process.env,
): Effect.Effect<CommandPlan, StateError> =>
  Effect.gen(function* () {
    const chain = yield* resolveProviderChain(request.action, environment);
    let plan: CommandPlan | null = null;
    for (const provider of chain) {
      plan = yield* runProviderStage(provider, request, plan, environment);
    }
    if (!plan)
      return yield* new StateError({
        message: `provider chain for ${request.action} produced no command`,
      });
    return plan;
  });

export const invokeProvider = (
  request: ProviderRequest,
  environment: Readonly<Record<string, string | undefined>> = process.env,
  stdio: "capture" | "inherit" = "capture",
): Effect.Effect<ProviderResponse, StateError> =>
  Effect.gen(function* () {
    const plan = yield* resolveCommandPlan(request, environment);
    return yield* Effect.tryPromise({
      try: async () => {
        const options = {
          cwd: plan.cwd ?? request.scope,
          env: { ...environment, ...plan.environment },
        };
        if (stdio === "inherit") {
          const child = Bun.spawn([...plan.command], {
            ...options,
            stderr: "inherit",
            stdin: "inherit",
            stdout: "inherit",
          });
          return { code: await child.exited, stderr: "", stdout: "" };
        }
        const child = Bun.spawn([...plan.command], {
          ...options,
          stderr: "pipe",
          stdin: "ignore",
          stdout: "pipe",
        });
        const [stdout, stderr, code] = await Promise.all([
          new Response(child.stdout).text(),
          new Response(child.stderr).text(),
          child.exited,
        ]);
        return { code, stderr, stdout };
      },
      catch: (cause) =>
        new StateError({
          message: `execute provider plan for ${request.action}`,
          cause,
        }),
    });
  });

export const providerOutput = (
  request: ProviderRequest,
  environment: Readonly<Record<string, string | undefined>> = process.env,
): Effect.Effect<string, StateError> =>
  invokeProvider(request, environment).pipe(
    Effect.flatMap((response) =>
      response.code === 0
        ? Effect.succeed(response.stdout.trimEnd())
        : Effect.fail(
            new StateError({
              message:
                response.stderr.trim() ||
                `provider action ${request.action} exited with code ${response.code}`,
            }),
          ),
    ),
  );
