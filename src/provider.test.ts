import { describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Effect, Exit } from "effect";
import type { Session } from "./domain.ts";
import {
  type CommandPlan,
  describeSession,
  discoverSessionBindings,
  listProviders,
  type ProviderCapability,
  providerOutput,
  resolveCommandPlan,
  resolveProviderChain,
} from "./provider.ts";

const changesRequest = (scope: string) => ({
  action: "changes" as const,
  scope,
  version: "orc.provider/v1" as const,
});

const session = (scope: string): Session => ({
  completion: "orchestrator",
  connectedAt: new Date(0).toISOString(),
  directory: scope,
  expectedOutput: "output",
  goal: "goal",
  harness: "test",
  id: "test-session",
  model: null,
  nativeId: "native-session",
  nodeId: null,
  parentId: null,
  providerRef: "managed-session",
  purpose: "test providers",
  providers: [],
  registration: "managed",
  reviewBy: null,
  role: "worker",
  runId: null,
  status: "working",
  successCriteria: [],
  title: "test",
  traceId: "trace-session",
  updatedAt: new Date(0).toISOString(),
});

const shellQuote = (value: string): string =>
  `'${value.replaceAll("'", `'\\''`)}'`;

const writeProvider = async (
  directory: string,
  options: {
    readonly capabilities: ReadonlyArray<ProviderCapability>;
    readonly capture?: string;
    readonly name: string;
    readonly plan: CommandPlan;
    readonly priority?: number;
  },
): Promise<string> => {
  return writeProviderResponse(directory, {
    ...options,
    response: options.plan,
  });
};

const writeProviderResponse = async (
  directory: string,
  options: {
    readonly capabilities: ReadonlyArray<ProviderCapability>;
    readonly capture?: string;
    readonly kind?: string;
    readonly name: string;
    readonly priority?: number;
    readonly response: unknown;
  },
): Promise<string> => {
  const executable = join(directory, `provider-${options.name}`);
  const capture = options.capture
    ? `request=$(cat)\nprintf %s "$request" > ${shellQuote(options.capture)}\n`
    : "cat >/dev/null\n";
  await writeFile(
    executable,
    `#!/bin/sh\nset -eu\n${capture}printf %s ${shellQuote(JSON.stringify(options.response))}\n`,
    { mode: 0o700 },
  );
  await writeFile(
    join(directory, `${options.name}.json`),
    JSON.stringify({
      capabilities: options.capabilities,
      command: executable,
      ...(options.kind ? { kind: options.kind } : {}),
      name: options.name,
      priority: options.priority ?? 0,
      version: "orc.provider/v1",
    }),
  );
  return executable;
};

const testEnvironment = (
  directory: string,
): Readonly<Record<string, string | undefined>> => ({
  ...process.env,
  ORC_PROVIDER_DIR: directory,
});

describe("provider", () => {
  test("discovers one capability provider and executes its command plan", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-provider-test-"));
    const capture = join(directory, "request.json");
    const printf = Bun.which("printf");
    if (!printf) throw new Error("test requires printf on PATH");
    await writeProvider(directory, {
      capabilities: ["changes.inspect"],
      capture,
      name: "local",
      plan: {
        command: [printf, "provider output"],
        version: "orc.provider/v1",
      },
    });
    try {
      expect(
        await Effect.runPromise(
          providerOutput(changesRequest(directory), testEnvironment(directory)),
        ),
      ).toBe("provider output");
      const received = JSON.parse(await readFile(capture, "utf8")) as {
        readonly action?: unknown;
        readonly capability?: unknown;
        readonly plan?: unknown;
        readonly version?: unknown;
      };
      expect(received).toMatchObject({
        action: "changes",
        capability: "changes.inspect",
        plan: null,
        version: "orc.provider/v1",
      });
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("passes one provider plan into the next capability", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-provider-chain-"));
    const inspectCapture = join(directory, "inspect.json");
    const terminalCapture = join(directory, "terminal.json");
    const printf = Bun.which("printf");
    if (!printf) throw new Error("test requires printf on PATH");
    const inspectPlan: CommandPlan = {
      command: [printf, "inspect"],
      cwd: directory,
      version: "orc.provider/v1",
    };
    const terminalPlan: CommandPlan = {
      command: [printf, "wrapped"],
      cwd: directory,
      version: "orc.provider/v1",
    };
    await writeProvider(directory, {
      capabilities: ["session.inspect"],
      capture: inspectCapture,
      name: "inspector",
      plan: inspectPlan,
    });
    await writeProvider(directory, {
      capabilities: ["terminal.open"],
      capture: terminalCapture,
      name: "terminal",
      plan: terminalPlan,
    });
    try {
      const request = {
        action: "inspect" as const,
        direction: "right" as const,
        scope: directory,
        session: session(directory),
        version: "orc.provider/v1" as const,
      };
      expect(
        await Effect.runPromise(
          providerOutput(request, testEnvironment(directory)),
        ),
      ).toBe("wrapped");
      const terminalRequest = JSON.parse(
        await readFile(terminalCapture, "utf8"),
      ) as { readonly capability?: unknown; readonly plan?: unknown };
      expect(terminalRequest).toMatchObject({
        capability: "terminal.open",
        plan: inspectPlan,
      });
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("composes harness, persistence, and display providers", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-provider-attach-"));
    const persistenceCapture = join(directory, "persistence.json");
    const terminalCapture = join(directory, "terminal.json");
    const printf = Bun.which("printf");
    if (!printf) throw new Error("test requires printf on PATH");
    const harnessPlan: CommandPlan = {
      command: [printf, "harness"],
      version: "orc.provider/v1",
    };
    const persistencePlan: CommandPlan = {
      command: [printf, "persisted"],
      version: "orc.provider/v1",
    };
    await writeProvider(directory, {
      capabilities: ["session.attach"],
      name: "harness",
      plan: harnessPlan,
    });
    await writeProvider(directory, {
      capabilities: ["session.persist"],
      capture: persistenceCapture,
      name: "persistence",
      plan: persistencePlan,
    });
    await writeProvider(directory, {
      capabilities: ["terminal.open"],
      capture: terminalCapture,
      name: "display",
      plan: { command: [printf, "displayed"], version: "orc.provider/v1" },
    });
    try {
      await Effect.runPromise(
        providerOutput(
          {
            action: "attach",
            direction: "right",
            scope: directory,
            session: session(directory),
            version: "orc.provider/v1",
          },
          testEnvironment(directory),
        ),
      );
      expect(
        JSON.parse(await readFile(persistenceCapture, "utf8")),
      ).toMatchObject({ plan: harnessPlan });
      expect(JSON.parse(await readFile(terminalCapture, "utf8"))).toMatchObject(
        { plan: persistencePlan },
      );
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("selects the highest-priority provider", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-provider-priority-"));
    const printf = Bun.which("printf");
    if (!printf) throw new Error("test requires printf on PATH");
    await writeProvider(directory, {
      capabilities: ["changes.inspect"],
      name: "fallback",
      plan: { command: [printf, "fallback"], version: "orc.provider/v1" },
      priority: 1,
    });
    await writeProvider(directory, {
      capabilities: ["changes.inspect"],
      name: "preferred",
      plan: { command: [printf, "preferred"], version: "orc.provider/v1" },
      priority: 10,
    });
    try {
      const chain = await Effect.runPromise(
        resolveProviderChain("changes", testEnvironment(directory)),
      );
      expect(chain.map((provider) => provider.name)).toEqual(["preferred"]);
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("falls back when a higher-priority provider declines", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-provider-fallback-"));
    const printf = Bun.which("printf");
    if (!printf) throw new Error("test requires printf on PATH");
    await writeProviderResponse(directory, {
      capabilities: ["changes.inspect"],
      name: "optional",
      priority: 20,
      response: { status: "declined", version: "orc.provider/v1" },
    });
    await writeProvider(directory, {
      capabilities: ["changes.inspect"],
      name: "fallback",
      plan: { command: [printf, "fallback"], version: "orc.provider/v1" },
      priority: 10,
    });
    try {
      expect(
        await Effect.runPromise(
          providerOutput(changesRequest(directory), testEnvironment(directory)),
        ),
      ).toBe("fallback");
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("discovers provider facets and session descriptions", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-provider-bind-"));
    await writeProviderResponse(directory, {
      capabilities: ["session.bind"],
      kind: "display",
      name: "display",
      response: {
        binding: {
          kind: "display",
          label: "pane 7",
          ref: "7",
          status: "active",
        },
        version: "orc.provider/v1",
      },
    });
    await writeProviderResponse(directory, {
      capabilities: ["session.describe"],
      kind: "activity",
      name: "activity",
      response: {
        description: {
          goal: "Build the provider model",
          title: "Orc providers",
        },
        version: "orc.provider/v1",
      },
    });
    try {
      expect(
        await Effect.runPromise(
          discoverSessionBindings(
            directory,
            session(directory),
            testEnvironment(directory),
          ),
        ),
      ).toEqual([
        {
          kind: "display",
          label: "pane 7",
          provider: "display",
          ref: "7",
          status: "active",
        },
      ]);
      expect(
        await Effect.runPromise(
          describeSession(
            directory,
            session(directory),
            testEnvironment(directory),
          ),
        ),
      ).toEqual({ goal: "Build the provider model", title: "Orc providers" });
      expect(
        (
          await Effect.runPromise(listProviders(testEnvironment(directory)))
        ).map(({ kind, name }) => ({ kind, name })),
      ).toEqual([
        { kind: "activity", name: "activity" },
        { kind: "display", name: "display" },
      ]);
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("reports a missing capability", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-provider-missing-"));
    try {
      const exit = await Effect.runPromiseExit(
        resolveCommandPlan(
          changesRequest(directory),
          testEnvironment(directory),
        ),
      );
      expect(Exit.isFailure(exit)).toBe(true);
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("reports providers tied at the highest priority", async () => {
    const directory = await mkdtemp(join(tmpdir(), "orc-provider-tie-"));
    const printf = Bun.which("printf");
    if (!printf) throw new Error("test requires printf on PATH");
    for (const name of ["first", "second"]) {
      await writeProvider(directory, {
        capabilities: ["changes.inspect"],
        name,
        plan: { command: [printf, name], version: "orc.provider/v1" },
        priority: 5,
      });
    }
    try {
      const exit = await Effect.runPromiseExit(
        resolveProviderChain("changes", testEnvironment(directory)),
      );
      expect(Exit.isFailure(exit)).toBe(true);
    } finally {
      await rm(directory, { force: true, recursive: true });
    }
  });

  test("keeps host tool names outside Orc core", async () => {
    const files = [
      "src/args.ts",
      "src/control.ts",
      "src/domain.ts",
      "src/hook.ts",
      "src/provider.ts",
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
