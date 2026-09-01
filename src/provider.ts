import { homedir } from "node:os";
import { isAbsolute, join, sep } from "node:path";
import { Effect } from "effect";
import type { Direction } from "./args.ts";
import type { Session } from "./domain.ts";
import { StateError } from "./state.ts";

export type ProviderAction = "attach" | "inspect" | "changes" | "launch";

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

export interface ProviderResponse {
  readonly code: number;
  readonly stderr: string;
  readonly stdout: string;
}

interface ProviderDocument {
  readonly providers: Partial<Record<ProviderAction, string>>;
}

const record = (value: unknown): Readonly<Record<string, unknown>> | null =>
  typeof value === "object" && value !== null
    ? (value as Readonly<Record<string, unknown>>)
    : null;

const providerConfigPath = (
  environment: Readonly<Record<string, string | undefined>>,
): string =>
  environment.ORC_PROVIDER_CONFIG ??
  join(
    environment.XDG_CONFIG_HOME ??
      join(environment.HOME ?? homedir(), ".config"),
    "orc",
    "providers.json",
  );

const readProviderDocument = (
  environment: Readonly<Record<string, string | undefined>>,
): Effect.Effect<ProviderDocument, StateError> =>
  Effect.tryPromise({
    try: async () => {
      const file = Bun.file(providerConfigPath(environment));
      if (!(await file.exists())) return { providers: {} };
      const parsed = record(await file.json());
      const providers = record(parsed?.providers);
      if (!parsed || !providers) throw new Error("providers must be an object");
      const selected: Partial<Record<ProviderAction, string>> = {};
      for (const action of [
        "attach",
        "inspect",
        "changes",
        "launch",
      ] as const) {
        const value = providers[action];
        if (value === undefined) continue;
        if (typeof value !== "string" || value.trim().length === 0)
          throw new Error(`providers.${action} must be a non-empty string`);
        selected[action] = value.trim();
      }
      return { providers: selected };
    },
    catch: (cause) =>
      new StateError({
        message: `read provider config ${providerConfigPath(environment)}`,
        cause,
      }),
  });

const providerName = (
  action: ProviderAction,
  environment: Readonly<Record<string, string | undefined>>,
): Effect.Effect<string, StateError> =>
  Effect.gen(function* () {
    const override = environment[`ORC_PROVIDER_${action.toUpperCase()}`];
    if (override?.trim()) return override.trim();
    const configured = (yield* readProviderDocument(environment)).providers[
      action
    ];
    const name = configured || environment.ORC_PROVIDER?.trim();
    if (!name)
      return yield* new StateError({
        message: `no provider is configured for ${action}`,
      });
    return name;
  });

export const resolveProvider = (
  action: ProviderAction,
  environment: Readonly<Record<string, string | undefined>> = process.env,
): Effect.Effect<string, StateError> =>
  Effect.gen(function* () {
    const name = yield* providerName(action, environment);
    const binary =
      isAbsolute(name) || name.includes(sep) ? name : `orc-${name}`;
    const executable = Bun.which(
      binary,
      environment.PATH ? { PATH: environment.PATH } : undefined,
    );
    if (!executable)
      return yield* new StateError({
        message: `provider ${name} for ${action} was not found on PATH`,
      });
    return executable;
  });

export const invokeProvider = (
  request: ProviderRequest,
  environment: Readonly<Record<string, string | undefined>> = process.env,
  stdio: "capture" | "inherit" = "capture",
): Effect.Effect<ProviderResponse, StateError> =>
  Effect.gen(function* () {
    const executable = yield* resolveProvider(request.action, environment);
    return yield* Effect.tryPromise({
      try: async () => {
        const env = {
          ...environment,
          ORC_PROVIDER_REQUEST: JSON.stringify(request),
        };
        if (stdio === "inherit") {
          const child = Bun.spawn([executable, request.action], {
            cwd: request.scope,
            env,
            stderr: "inherit",
            stdin: "inherit",
            stdout: "inherit",
          });
          return { code: await child.exited, stderr: "", stdout: "" };
        }
        const child = Bun.spawn([executable, request.action], {
          cwd: request.scope,
          env,
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
          message: `run provider action ${request.action}`,
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
