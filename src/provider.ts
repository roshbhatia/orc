import type { Dirent } from "node:fs";
import { readdir } from "node:fs/promises";
import { homedir } from "node:os";
import { extname, isAbsolute, join, sep } from "node:path";
import { Effect } from "effect";
import type { Direction } from "./args.ts";
import type { ProviderBinding, ProviderKind, Session } from "./domain.ts";
import { StateError } from "./state.ts";

export type ProviderAction =
  | "activity"
  | "attach"
  | "inspect"
  | "changes"
  | "launch";

export type ProviderCapability =
  | "changes.inspect"
  | "session.attach"
  | "session.bind"
  | "session.describe"
  | "session.inspect"
  | "session.launch"
  | "session.persist"
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
      readonly action: "activity";
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

type ProviderQueryRequest = ProviderRequestBase & {
  readonly action: "bind" | "describe";
  readonly session: Session;
};

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

export interface ProviderInfo {
  readonly capabilities: ReadonlyArray<ProviderCapability>;
  readonly command: string;
  readonly kind: ProviderKind;
  readonly name: string;
  readonly priority: number;
}

export interface ResolvedProvider extends ProviderInfo {
  readonly capability: ProviderCapability;
}

export interface SessionDescription {
  readonly goal?: string;
  readonly title?: string;
}

interface ProviderManifest extends ProviderInfo {
  readonly version: "orc.provider/v1";
}

type ProviderStageRequest = (ProviderRequest | ProviderQueryRequest) & {
  readonly capability: ProviderCapability;
  readonly plan: CommandPlan | null;
};

const capabilities = [
  "changes.inspect",
  "session.attach",
  "session.bind",
  "session.describe",
  "session.inspect",
  "session.launch",
  "session.persist",
  "terminal.open",
] as const;

const providerKinds: ReadonlyArray<ProviderKind> = [
  "persistence",
  "display",
  "activity",
  "changes",
  "harness",
  "integration",
];

interface CapabilityStage {
  readonly capability: ProviderCapability;
  readonly optional?: boolean;
}

const actionCapabilities: Readonly<
  Record<ProviderAction, ReadonlyArray<CapabilityStage>>
> = {
  activity: [{ capability: "session.inspect" }],
  attach: [
    { capability: "session.attach" },
    { capability: "session.persist", optional: true },
    { capability: "terminal.open" },
  ],
  changes: [{ capability: "changes.inspect" }],
  inspect: [{ capability: "session.inspect" }, { capability: "terminal.open" }],
  launch: [{ capability: "session.launch" }],
};

const record = (value: unknown): Readonly<Record<string, unknown>> | null =>
  typeof value === "object" && value !== null
    ? (value as Readonly<Record<string, unknown>>)
    : null;

const isCapability = (value: string): value is ProviderCapability =>
  capabilities.some((capability) => capability === value);

const isProviderKind = (value: string): value is ProviderKind =>
  providerKinds.some((kind) => kind === value);

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
  const kind = parsed.kind ?? "integration";
  if (typeof kind !== "string" || !isProviderKind(kind))
    throw new Error(`${path}: unsupported provider kind ${String(kind)}`);
  return {
    capabilities: selected,
    command,
    kind,
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

export const listProviders = (
  environment: Readonly<Record<string, string | undefined>> = process.env,
): Effect.Effect<ReadonlyArray<ProviderInfo>, StateError> =>
  discoverProviders(environment).pipe(
    Effect.map((providers) =>
      providers.map(({ version: _version, ...provider }) => provider),
    ),
  );

const candidatesFrom = (
  providers: ReadonlyArray<ProviderManifest>,
  capability: ProviderCapability,
): ReadonlyArray<ResolvedProvider> =>
  providers
    .filter((provider) => provider.capabilities.includes(capability))
    .sort(
      (left, right) =>
        right.priority - left.priority || left.name.localeCompare(right.name),
    )
    .map((provider) => ({ ...provider, capability }));

const candidatesFor = (
  capability: ProviderCapability,
  environment: Readonly<Record<string, string | undefined>>,
): Effect.Effect<ReadonlyArray<ResolvedProvider>, StateError> =>
  discoverProviders(environment).pipe(
    Effect.map((providers) => candidatesFrom(providers, capability)),
  );

export const resolveProviderChain = (
  action: ProviderAction,
  environment: Readonly<Record<string, string | undefined>> = process.env,
): Effect.Effect<ReadonlyArray<ResolvedProvider>, StateError> =>
  Effect.gen(function* () {
    const providers = yield* discoverProviders(environment);
    const chain: ResolvedProvider[] = [];
    for (const stage of actionCapabilities[action]) {
      const capability = stage.capability;
      const candidates = candidatesFrom(providers, capability);
      const selected = candidates[0];
      if (!selected && stage.optional) continue;
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
      chain.push(selected);
    }
    return chain;
  });

const timeoutOf = (
  environment: Readonly<Record<string, string | undefined>>,
): number => {
  const parsed = Number(environment.ORC_PROVIDER_TIMEOUT_MS ?? "5000");
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : 5000;
};

const runProvider = (
  provider: ResolvedProvider,
  request: ProviderRequest | ProviderQueryRequest,
  plan: CommandPlan | null,
  environment: Readonly<Record<string, string | undefined>>,
): Effect.Effect<unknown, StateError> =>
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
      const outcome = Promise.all([
        new Response(child.stdout).text(),
        new Response(child.stderr).text(),
        child.exited,
      ]);
      const [stdout, stderr, code] = await Promise.race([
        outcome,
        Bun.sleep(timeoutOf(environment)).then(() => {
          child.kill();
          throw new Error(`${provider.name} timed out`);
        }),
      ]);
      if (code !== 0)
        throw new Error(
          stderr.trim() || `${provider.name} exited with code ${code}`,
        );
      try {
        return JSON.parse(stdout) as unknown;
      } catch (cause) {
        throw new Error(`${provider.name} returned invalid JSON`, { cause });
      }
    },
    catch: (cause) =>
      new StateError({
        message: `run ${provider.name} for ${provider.capability}`,
        cause,
      }),
  });

const declined = (value: unknown): boolean =>
  record(value)?.status === "declined";

const parseCommandPlan = (
  value: unknown,
  provider: string,
): CommandPlan | null => {
  if (declined(value)) return null;
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

export const resolveCommandPlan = (
  request: ProviderRequest,
  environment: Readonly<Record<string, string | undefined>> = process.env,
): Effect.Effect<CommandPlan, StateError> =>
  Effect.gen(function* () {
    const providers = yield* discoverProviders(environment);
    let plan: CommandPlan | null = null;
    for (const stage of actionCapabilities[request.action]) {
      const capability = stage.capability;
      const candidates = candidatesFrom(providers, capability);
      if (candidates.length === 0 && stage.optional) continue;
      if (candidates.length === 0)
        return yield* new StateError({
          message: `no provider advertises capability ${capability}`,
        });
      let accepted: CommandPlan | null = null;
      for (const provider of candidates) {
        accepted = parseCommandPlan(
          yield* runProvider(provider, request, plan, environment),
          provider.name,
        );
        if (accepted) break;
      }
      if (!accepted)
        return yield* new StateError({
          message: `all providers declined capability ${capability}`,
        });
      plan = accepted;
    }
    if (!plan)
      return yield* new StateError({
        message: `provider chain for ${request.action} produced no command`,
      });
    return plan;
  });

const parseBinding = (
  value: unknown,
  provider: ResolvedProvider,
): ProviderBinding | null => {
  if (declined(value)) return null;
  const parsed = record(value);
  const binding = record(parsed?.binding);
  if (parsed?.version !== "orc.provider/v1" || !binding)
    throw new Error(`${provider.name}: response must contain a binding`);
  const kind = binding.kind;
  const status = binding.status;
  if (typeof kind !== "string" || !isProviderKind(kind))
    throw new Error(`${provider.name}: binding kind is invalid`);
  if (status !== "active" && status !== "available" && status !== "unavailable")
    throw new Error(`${provider.name}: binding status is invalid`);
  if (binding.ref !== null && typeof binding.ref !== "string")
    throw new Error(`${provider.name}: binding ref is invalid`);
  if (typeof binding.label !== "string")
    throw new Error(`${provider.name}: binding label is invalid`);
  return {
    kind,
    label: binding.label,
    provider: provider.name,
    ref: binding.ref,
    status,
  };
};

export const discoverSessionBindings = (
  scope: string,
  session: Session,
  environment: Readonly<Record<string, string | undefined>> = process.env,
): Effect.Effect<ReadonlyArray<ProviderBinding>, StateError> =>
  Effect.gen(function* () {
    const candidates = yield* candidatesFor("session.bind", environment);
    const bindings: ProviderBinding[] = [];
    for (const provider of candidates) {
      const binding = parseBinding(
        yield* runProvider(
          provider,
          {
            action: "bind",
            scope,
            session,
            version: "orc.provider/v1",
          },
          null,
          environment,
        ),
        provider,
      );
      if (
        binding &&
        !bindings.some(
          (candidate) =>
            candidate.provider === binding.provider &&
            candidate.kind === binding.kind,
        )
      )
        bindings.push(binding);
    }
    return bindings;
  });

const parseDescription = (
  value: unknown,
  provider: string,
): SessionDescription | null => {
  if (declined(value)) return null;
  const parsed = record(value);
  const description = record(parsed?.description);
  if (parsed?.version !== "orc.provider/v1" || !description)
    throw new Error(`${provider}: response must contain a description`);
  const title = description.title;
  const goal = description.goal;
  if (title !== undefined && typeof title !== "string")
    throw new Error(`${provider}: description title is invalid`);
  if (goal !== undefined && typeof goal !== "string")
    throw new Error(`${provider}: description goal is invalid`);
  return {
    ...(typeof goal === "string" && goal.length > 0 ? { goal } : {}),
    ...(typeof title === "string" && title.length > 0 ? { title } : {}),
  };
};

export const describeSession = (
  scope: string,
  session: Session,
  environment: Readonly<Record<string, string | undefined>> = process.env,
): Effect.Effect<SessionDescription, StateError> =>
  Effect.gen(function* () {
    const candidates = yield* candidatesFor("session.describe", environment);
    const description: { goal?: string; title?: string } = {};
    for (const provider of candidates) {
      const result = parseDescription(
        yield* runProvider(
          provider,
          {
            action: "describe",
            scope,
            session,
            version: "orc.provider/v1",
          },
          null,
          environment,
        ),
        provider.name,
      );
      if (!result) continue;
      if (!description.title && result.title) description.title = result.title;
      if (!description.goal && result.goal) description.goal = result.goal;
    }
    return description;
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
