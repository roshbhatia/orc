import { join } from "node:path";
import {
  configPath,
  configSchema,
  loadConfig,
} from "@roshbhatia/ts-utils/config";
import { Schema } from "effect";

export const OrcConfigSchema = Schema.Struct({
  cache: Schema.Struct({
    providerTtlMs: Schema.Int,
  }),
  providers: Schema.Struct({
    directory: Schema.String,
    timeoutMs: Schema.Int,
  }),
});

export type OrcConfig = typeof OrcConfigSchema.Type;

const providerDirectory = (
  environment: Readonly<Record<string, string | undefined>>,
): string =>
  join(
    environment.XDG_CONFIG_HOME ??
      join(environment.HOME ?? process.cwd(), ".config"),
    "orc",
    "providers",
  );

const normalizedEnvironment = (
  environment: Readonly<Record<string, string | undefined>>,
): Readonly<Record<string, string | undefined>> => ({
  ...environment,
  ORC_PROVIDERS_DIRECTORY:
    environment.ORC_PROVIDERS_DIRECTORY ?? environment.ORC_PROVIDER_DIR,
  ORC_PROVIDERS_TIMEOUT_MS:
    environment.ORC_PROVIDERS_TIMEOUT_MS ?? environment.ORC_PROVIDER_TIMEOUT_MS,
});

export const loadOrcConfig = (
  environment: Readonly<Record<string, string | undefined>> = process.env,
): OrcConfig => {
  const selected = normalizedEnvironment(environment);
  const config = loadConfig(OrcConfigSchema, {
    defaults: {
      cache: { providerTtlMs: 1_000 },
      providers: {
        directory: providerDirectory(selected),
        timeoutMs: 5_000,
      },
    },
    environment: selected,
    name: "orc",
    prefix: "ORC",
  });
  if (
    !Number.isSafeInteger(config.cache.providerTtlMs) ||
    config.cache.providerTtlMs < 0
  )
    throw new Error("cache.providerTtlMs must be a non-negative integer");
  if (
    !Number.isSafeInteger(config.providers.timeoutMs) ||
    config.providers.timeoutMs <= 0
  )
    throw new Error("providers.timeoutMs must be a positive integer");
  if (config.providers.directory.trim().length === 0)
    throw new Error("providers.directory must be non-empty");
  return config;
};

export const orcConfigPath = (
  environment: Readonly<Record<string, string | undefined>> = process.env,
): string =>
  configPath({
    environment: normalizedEnvironment(environment),
    name: "orc",
    prefix: "ORC",
  });

export const orcConfigSchema = (): Readonly<Record<string, unknown>> =>
  configSchema(OrcConfigSchema, "Orc configuration");
