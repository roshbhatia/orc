import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { loadOrcConfig } from "./config.ts";
import type { ProviderRequest } from "./provider.ts";

interface CacheEntry {
  readonly output: string;
  readonly writtenAt: number;
}

const cacheDirectory = (
  environment: Readonly<Record<string, string | undefined>>,
): string =>
  join(
    environment.XDG_CACHE_HOME ??
      join(environment.HOME ?? process.cwd(), ".cache"),
    "orc",
    "providers",
  );

const cachePath = (
  request: ProviderRequest,
  environment: Readonly<Record<string, string | undefined>>,
): string => {
  const digest = createHash("sha256")
    .update(
      JSON.stringify({
        providerDirectory: loadOrcConfig(environment).providers.directory,
        request,
      }),
    )
    .digest("hex");
  return join(cacheDirectory(environment), `${digest}.json`);
};

export const readProviderCache = async (
  request: ProviderRequest,
  environment: Readonly<Record<string, string | undefined>>,
): Promise<string | null> => {
  const ttl = loadOrcConfig(environment).cache.providerTtlMs;
  if (ttl === 0) return null;
  try {
    const entry = JSON.parse(
      await readFile(cachePath(request, environment), "utf8"),
    ) as CacheEntry;
    return Date.now() - entry.writtenAt <= ttl ? entry.output : null;
  } catch (cause) {
    if (
      typeof cause === "object" &&
      cause !== null &&
      "code" in cause &&
      cause.code === "ENOENT"
    )
      return null;
    return null;
  }
};

export const writeProviderCache = async (
  request: ProviderRequest,
  environment: Readonly<Record<string, string | undefined>>,
  output: string,
): Promise<void> => {
  if (loadOrcConfig(environment).cache.providerTtlMs === 0) return;
  const path = cachePath(request, environment);
  await mkdir(cacheDirectory(environment), { recursive: true });
  const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
  await writeFile(
    temporary,
    `${JSON.stringify({ output, writtenAt: Date.now() } satisfies CacheEntry)}\n`,
  );
  await rename(temporary, path);
};
