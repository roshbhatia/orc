import { describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { Effect, Exit } from "effect";
import { providerOutput, resolveProvider } from "./provider.ts";

const request = (scope: string) => ({
  action: "changes" as const,
  scope,
  version: "orc.provider/v1" as const,
});

describe("provider", () => {
  test("routes one action through the configured provider", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-provider-test-"));
    const executable = join(directory, "orc-local");
    const capture = join(directory, "request.json");
    const config = join(directory, "providers.json");
    await writeFile(
      executable,
      '#!/bin/sh\nprintf "%s" "$ORC_PROVIDER_REQUEST" > "$ORC_PROVIDER_CAPTURE"\nprintf "provider output"\n',
      { mode: 0o700 },
    );
    await writeFile(
      config,
      JSON.stringify({ providers: { changes: "local" } }),
    );
    const environment = {
      ...process.env,
      ORC_PROVIDER_CAPTURE: capture,
      ORC_PROVIDER_CONFIG: config,
      PATH: `${directory}${delimiter}${process.env.PATH ?? ""}`,
    };
    try {
      expect(
        await Effect.runPromise(
          providerOutput(request(directory), environment),
        ),
      ).toBe("provider output");
      const received = JSON.parse(await readFile(capture, "utf8")) as {
        readonly action?: unknown;
        readonly version?: unknown;
      };
      expect(received).toMatchObject({
        action: "changes",
        version: "orc.provider/v1",
      });
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("lets one action override the common provider", async () => {
    const shell = Bun.which("sh");
    if (!shell) throw new Error("test requires sh on PATH");
    const environment = {
      ...process.env,
      ORC_PROVIDER: "missing-common-provider",
      ORC_PROVIDER_CHANGES: shell,
      ORC_PROVIDER_CONFIG: join(tmpdir(), crypto.randomUUID()),
    };
    expect(
      await Effect.runPromise(resolveProvider("changes", environment)),
    ).toBe(shell);
  });

  test("reports an unavailable provider", async () => {
    const environment = {
      ...process.env,
      ORC_PROVIDER: undefined,
      ORC_PROVIDER_CHANGES: undefined,
      ORC_PROVIDER_CONFIG: join(tmpdir(), crypto.randomUUID()),
    };
    const exit = await Effect.runPromiseExit(
      providerOutput(request(process.cwd()), environment),
    );
    expect(Exit.isFailure(exit)).toBe(true);
  });

  test("keeps host tool names outside Orc core", async () => {
    const files = [
      "README.md",
      "src/args.ts",
      "src/control.ts",
      "src/domain.ts",
      "src/hook.ts",
      "src/run.ts",
      "src/tui.tsx",
    ];
    const forbidden = [
      ["wez", "term"].join(""),
      ["z", "mx"].join(""),
      ["tra", "ces"].join(""),
    ];
    for (const file of files) {
      const contents = (await readFile(file, "utf8")).toLowerCase();
      for (const name of forbidden) expect(contents).not.toContain(name);
    }
  });
});
