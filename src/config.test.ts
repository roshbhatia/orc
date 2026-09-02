import { describe, expect, test } from "bun:test";
import { mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { loadOrcConfig, orcConfigPath, orcConfigSchema } from "./config.ts";

describe("configuration", () => {
  test("loads YAML and nested environment overrides", async () => {
    const home = join(tmpdir(), `orc-config-${crypto.randomUUID()}`);
    const environment = {
      HOME: home,
      ORC_CACHE_PROVIDER_TTL_MS: "250",
      ORC_PROVIDERS_TIMEOUT_MS: "9000",
    };
    const path = orcConfigPath(environment);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(
      path,
      "cache:\n  providerTtlMs: 500\nproviders:\n  directory: /tmp/orc-providers\n  timeoutMs: 7000\n",
    );
    try {
      expect(loadOrcConfig(environment)).toEqual({
        cache: { providerTtlMs: 250 },
        providers: {
          directory: "/tmp/orc-providers",
          timeoutMs: 9000,
        },
      });
    } finally {
      await rm(home, { force: true, recursive: true });
    }
  });

  test("supports the original provider environment names", () => {
    expect(
      loadOrcConfig({
        HOME: "/tmp/orc-home",
        ORC_PROVIDER_DIR: "/tmp/legacy-providers",
        ORC_PROVIDER_TIMEOUT_MS: "8000",
      }),
    ).toMatchObject({
      providers: {
        directory: "/tmp/legacy-providers",
        timeoutMs: 8000,
      },
    });
  });

  test("generates a draft 2020-12 schema", () => {
    expect(orcConfigSchema()).toMatchObject({
      $schema: "https://json-schema.org/draft/2020-12/schema",
      title: "Orc configuration",
      type: "object",
    });
  });
});
