import { Effect } from "effect";
import { type CompletionShell, parseArgs } from "./args.ts";
import {
  attach,
  connect,
  createRun,
  currentSession,
  disconnect,
  inspectSession,
  launch,
  readWorkspace,
  reconcileWorkspace,
  setRunAgent,
  updateNodeStatus,
  updateRunStatus,
  updateSessionStatus,
  upsertNode,
} from "./control.ts";
import {
  activeSessions,
  runsByRecency,
  sessionsByRecency,
  type WorkspaceState,
} from "./domain.ts";
import { registerFromHook } from "./hook.ts";
import { runMcp } from "./mcp.ts";
import { listProviders, providerOutput } from "./provider.ts";
import { StateError, StateStoreLive } from "./state.ts";
import { openTui } from "./tui.tsx";

export interface Streams {
  readonly stdout: (value: string) => void;
  readonly stderr: (value: string) => void;
}

interface CliOption {
  readonly description: string;
  readonly flag?: boolean;
  readonly name: string;
  readonly values?: ReadonlyArray<string>;
}

interface CliCommand {
  readonly description: string;
  readonly name: string;
  readonly options?: ReadonlyArray<CliOption>;
  readonly usage?: string;
}

const scopeOption: CliOption = {
  description: "Select a workspace scope",
  name: "--scope",
};
const jsonOption: CliOption = {
  description: "Print JSON",
  flag: true,
  name: "--json",
};
const contractOptions: ReadonlyArray<CliOption> = [
  { description: "Select an agent harness", name: "--harness" },
  { description: "Select a harness model", name: "--model" },
  {
    description: "Set the agent role",
    name: "--role",
    values: [
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
    ],
  },
  { description: "Set the display title", name: "--title" },
  { description: "Explain why the agent exists", name: "--purpose" },
  { description: "Set the agent goal", name: "--goal" },
  { description: "Describe the expected output", name: "--expected-output" },
  { description: "Add one success criterion", name: "--success" },
  {
    description: "Select the completion target",
    name: "--completion",
    values: ["orchestrator", "judge"],
  },
  { description: "Set the review node", name: "--review-by" },
];
const statusOption: CliOption = {
  description: "Set lifecycle status",
  name: "--status",
  values: [
    "queued",
    "working",
    "waiting",
    "blocked",
    "failed",
    "done",
    "cancelled",
    "disconnected",
  ],
};

const cliCommands: ReadonlyArray<CliCommand> = [
  {
    description: "Open the control plane",
    name: "tui",
    options: [scopeOption],
  },
  {
    description: "Show workspace status",
    name: "status",
    options: [scopeOption, jsonOption],
  },
  {
    description: "List registered sessions",
    name: "list",
    options: [scopeOption, jsonOption],
  },
  {
    description: "List provider manifests",
    name: "providers",
    options: [scopeOption, jsonOption],
  },
  {
    description: "Register the current session",
    name: "connect",
    options: [
      scopeOption,
      ...contractOptions,
      { description: "Set the Orc session id", name: "--id" },
      { description: "Set the harness session id", name: "--native-id" },
      { description: "Set the parent session", name: "--parent" },
      {
        description: "Set the provider session reference",
        name: "--provider-ref",
      },
    ],
  },
  {
    description: "Manage sessions",
    name: "session",
    usage: "register|current|list|update",
  },
  {
    description: "Manage workflow runs",
    name: "run",
    usage: "create|agent|list|show|update",
  },
  {
    description: "Manage workflow nodes",
    name: "node",
    usage: "upsert|update",
  },
  {
    description: "Launch a managed harness",
    name: "launch",
    options: [
      scopeOption,
      { description: "Set the managed session id", name: "--managed" },
      { description: "Select a harness model", name: "--model" },
    ],
    usage: "<harness> [-- args]",
  },
  {
    description: "Attach through a session provider",
    name: "attach",
    options: [
      scopeOption,
      {
        description: "Select the split direction",
        name: "--direction",
        values: ["right", "left", "top", "bottom"],
      },
    ],
    usage: "<session-id>",
  },
  {
    description: "Inspect through a session provider",
    name: "inspect",
    options: [
      scopeOption,
      {
        description: "Select the split direction",
        name: "--direction",
        values: ["right", "left", "top", "bottom"],
      },
    ],
    usage: "<session-id>",
  },
  {
    description: "Disconnect a registered session",
    name: "disconnect",
    options: [scopeOption],
    usage: "[session-id]",
  },
  { description: "Run the MCP server", name: "mcp" },
  {
    description: "Print the prompt session marker",
    name: "prompt",
    options: [scopeOption],
  },
  {
    description: "Generate shell completions",
    name: "completion",
    usage: "bash|zsh|fish|nu",
  },
  { description: "Show command help", name: "help" },
  { description: "Show the Orc version", name: "version" },
];

const nestedCommands: Readonly<Record<string, ReadonlyArray<CliCommand>>> = {
  completion: [
    {
      description: "Generate Bash completions",
      name: "bash",
    },
    {
      description: "Generate Fish completions",
      name: "fish",
    },
    {
      description: "Generate Nushell completions",
      name: "nu",
    },
    {
      description: "Generate Zsh completions",
      name: "zsh",
    },
  ],
  node: [
    {
      description: "Create or replace a workflow node",
      name: "upsert",
      options: [
        scopeOption,
        { description: "Select the workflow run", name: "--run" },
        { description: "Set the linked session", name: "--session" },
        statusOption,
        { description: "Set the attempt number", name: "--attempt" },
        { description: "Add a dependency node", name: "--depends-on" },
        ...contractOptions,
      ],
    },
    {
      description: "Update a workflow node status",
      name: "update",
      options: [
        scopeOption,
        { description: "Select the workflow run", name: "--run" },
        statusOption,
      ],
    },
  ],
  run: [
    {
      description: "Create a workflow run",
      name: "create",
      options: [
        scopeOption,
        { description: "Set the run name", name: "--name" },
        { description: "Set the run goal", name: "--goal" },
        {
          description: "Describe the expected output",
          name: "--expected-output",
        },
        {
          description: "Select the orchestrator session",
          name: "--orchestrator",
        },
        { description: "Select the default harness", name: "--harness" },
        { description: "Select the default model", name: "--model" },
        jsonOption,
      ],
    },
    {
      description: "Set a run role harness",
      name: "agent",
      options: [scopeOption, ...contractOptions.slice(0, 3)],
    },
    {
      description: "List workflow runs",
      name: "list",
      options: [scopeOption, jsonOption],
    },
    {
      description: "Show one workflow run",
      name: "show",
      options: [scopeOption, jsonOption],
    },
    {
      description: "Update a workflow run status",
      name: "update",
      options: [scopeOption, statusOption],
    },
  ],
  session: [
    {
      description: "Register a session",
      name: "register",
      options: [
        scopeOption,
        ...contractOptions,
        { description: "Set the Orc session id", name: "--id" },
        { description: "Set the harness session id", name: "--native-id" },
        { description: "Set the parent session", name: "--parent" },
        { description: "Link a workflow run", name: "--run" },
        { description: "Link a workflow node", name: "--node" },
        {
          description: "Set the registration source",
          name: "--source",
          values: ["connected", "hook", "managed"],
        },
        {
          description: "Set the provider session reference",
          name: "--provider-ref",
        },
        {
          description: "Read session data from standard input",
          flag: true,
          name: "--hook-input",
        },
        { description: "Suppress the session id", flag: true, name: "--quiet" },
      ],
    },
    {
      description: "Show the current session",
      name: "current",
      options: [scopeOption, jsonOption],
    },
    {
      description: "List sessions",
      name: "list",
      options: [scopeOption, jsonOption],
    },
    {
      description: "Update a session status",
      name: "update",
      options: [scopeOption, statusOption],
    },
  ],
};

const escapeFish = (value: string): string => value.replaceAll("'", "\\'");

const fishOption = (
  command: string,
  option: CliOption,
): ReadonlyArray<string> => {
  const base = `complete -c orc -n '${escapeFish(command)}' -l ${option.name.slice(2)} -d '${escapeFish(option.description)}'`;
  if (option.flag) return [base];
  if (!option.values) return [`${base} -r`];
  return [`${base} -r -a '${option.values.map(escapeFish).join(" ")}'`];
};

export const fishCompletion = (): string => {
  const lines = [
    "complete -c orc -e",
    "complete -c orc -f",
    ...cliCommands.map(
      (command) =>
        `complete -c orc -n '__fish_use_subcommand' -a '${escapeFish(command.name)}' -d '${escapeFish(command.description)}'`,
    ),
  ];
  for (const command of cliCommands) {
    const condition = `__fish_seen_subcommand_from ${command.name}`;
    for (const option of command.options ?? []) {
      lines.push(...fishOption(condition, option));
    }
  }
  for (const [parent, children] of Object.entries(nestedCommands)) {
    const names = children.map((command) => command.name).join(" ");
    const parentCondition = `__fish_seen_subcommand_from ${parent}; and not __fish_seen_subcommand_from ${names}`;
    for (const command of children) {
      lines.push(
        `complete -c orc -n '${escapeFish(parentCondition)}' -a '${escapeFish(command.name)}' -d '${escapeFish(command.description)}'`,
      );
      const condition = `__fish_seen_subcommand_from ${parent}; and __fish_seen_subcommand_from ${command.name}`;
      for (const option of command.options ?? []) {
        lines.push(...fishOption(condition, option));
      }
    }
  }
  return lines.join("\n");
};

const allCommandNames = (): ReadonlyArray<string> => [
  ...cliCommands.map((command) => command.name),
  ...Object.entries(nestedCommands).flatMap(([parent, commands]) =>
    commands.map((command) => `${parent} ${command.name}`),
  ),
];

const allOptionNames = (): ReadonlyArray<string> => [
  ...new Set(
    [
      ...cliCommands.flatMap((command) => command.options ?? []),
      ...Object.values(nestedCommands).flatMap((commands) =>
        commands.flatMap((command) => command.options ?? []),
      ),
    ].map((option) => option.name),
  ),
];

export const bashCompletion = (): string =>
  `
_orc_complete() {
  local current="\${COMP_WORDS[COMP_CWORD]}"
  local words="${[...allCommandNames(), ...allOptionNames()].join(" ")}"
  COMPREPLY=($(compgen -W "$words" -- "$current"))
}
complete -F _orc_complete orc
`.trim();

const zshQuote = (value: string): string => value.replaceAll("'", "'\\''");

export const zshCompletion = (): string =>
  `
#compdef orc
_orc() {
  local -a commands options
  commands=(
${cliCommands.map((command) => `    '${zshQuote(command.name)}:${zshQuote(command.description)}'`).join("\n")}
  )
  options=(${allOptionNames().map(zshQuote).join(" ")})
  if (( CURRENT == 2 )); then
    _describe 'command' commands
  else
    compadd -- $options
  fi
}
compdef _orc orc
`.trim();

const nuFlag = (option: CliOption): string =>
  option.flag
    ? `  ${option.name}`
    : `  ${option.name}: ${option.values ? `string@"nu-complete orc ${option.name.slice(2)}"` : "string"}`;

export const nuCompletion = (): string => {
  const valueCompleters = [
    ...new Map(
      [
        ...cliCommands.flatMap((command) => command.options ?? []),
        ...Object.values(nestedCommands).flatMap((commands) =>
          commands.flatMap((command) => command.options ?? []),
        ),
      ]
        .filter((option) => option.values)
        .map((option) => [option.name, option] as const),
    ).values(),
  ].map(
    (option) =>
      `def "nu-complete orc ${option.name.slice(2)}" [] { [${option.values?.map((value) => JSON.stringify(value)).join(" ")}] }`,
  );
  const declarations = [
    ...cliCommands.map((command) => {
      const options = command.options ?? [];
      return `export extern "orc ${command.name}" [\n${options.map(nuFlag).join("\n")}\n  ...args: string\n]`;
    }),
    ...Object.entries(nestedCommands).flatMap(([parent, commands]) =>
      commands.map(
        (command) =>
          `export extern "orc ${parent} ${command.name}" [\n${(command.options ?? []).map(nuFlag).join("\n")}\n  ...args: string\n]`,
      ),
    ),
  ];
  return [...valueCompleters, ...declarations].join("\n\n");
};

export const completionText = (shell: CompletionShell): string => {
  if (shell === "bash") return bashCompletion();
  if (shell === "zsh") return zshCompletion();
  if (shell === "nu") return nuCompletion();
  return fishCompletion();
};

const helpOptions = [
  ...cliCommands.flatMap((command) => command.options ?? []),
  ...Object.values(nestedCommands).flatMap((commands) =>
    commands.flatMap((command) => command.options ?? []),
  ),
].filter(
  (option, index, options) =>
    options.findIndex((candidate) => candidate.name === option.name) === index,
);

export const help = [
  "orc: local control plane for agent harnesses",
  "",
  "usage:",
  "  orc",
  ...cliCommands.map(
    (command) =>
      `  orc ${command.name}${command.usage ? ` ${command.usage}` : ""}`,
  ),
  ...Object.entries(nestedCommands).flatMap(([parent, children]) =>
    children.map(
      (command) =>
        `  orc ${parent} ${command.name}${command.usage ? ` ${command.usage}` : ""}`,
    ),
  ),
  "",
  "options:",
  ...helpOptions.map(
    (option) =>
      `  ${option.name}${option.flag ? "" : option.values ? ` <${option.values.join("|")}>` : " <value>"}  ${option.description}`,
  ),
  "",
  "Orc activates with the first registered session and idles after the last disconnect.",
].join("\n");

const displayList = (state: WorkspaceState): string =>
  sessionsByRecency(state.sessions)
    .map(
      (session) =>
        `${session.id}\t${session.status}\t${session.role}\t${session.title}`,
    )
    .join("\n");

const program = (
  args: ReadonlyArray<string>,
  streams: Streams,
  version: string,
) =>
  Effect.gen(function* () {
    const command = yield* parseArgs(args);
    switch (command.tag) {
      case "help":
        streams.stdout(help);
        return 0;
      case "version":
        streams.stdout(version);
        return 0;
      case "completion":
        streams.stdout(completionText(command.shell));
        return 0;
      case "mcp":
        yield* Effect.tryPromise({
          try: runMcp,
          catch: (cause) =>
            new StateError({ message: "run MCP server", cause }),
        });
        return 0;
      case "prompt": {
        const state = yield* readWorkspace(command.scope);
        streams.stdout(currentSession(state) ? "|⚔|" : "");
        return 0;
      }
      case "status": {
        const state = yield* readWorkspace(command.scope);
        const result = {
          active: state.active,
          runs: state.runs.length,
          scope: state.scope,
          sessions: state.sessions.length,
          working: activeSessions(state).length,
        };
        streams.stdout(
          command.json
            ? JSON.stringify(result)
            : `${result.active ? "active" : "idle"} · ${result.working} working · ${result.sessions} sessions · ${result.runs} runs · ${result.scope}`,
        );
        return 0;
      }
      case "list":
      case "session-list": {
        const state = yield* readWorkspace(command.scope);
        streams.stdout(
          command.json ? JSON.stringify(state.sessions) : displayList(state),
        );
        return 0;
      }
      case "provider-list": {
        const providers = yield* listProviders();
        streams.stdout(
          command.json
            ? JSON.stringify(providers)
            : providers
                .map(
                  (provider) =>
                    `${provider.name}\t${provider.kind}\t${provider.capabilities.join(",")}`,
                )
                .join("\n"),
        );
        return 0;
      }
      case "session-current": {
        const session = currentSession(yield* readWorkspace(command.scope));
        if (!session) return 1;
        streams.stdout(command.json ? JSON.stringify(session) : session.id);
        return 0;
      }
      case "connect":
        streams.stdout((yield* connect(command)).id);
        return 0;
      case "session-register": {
        const session = yield* registerFromHook(command);
        if (session && !command.quiet) streams.stdout(session.id);
        return 0;
      }
      case "disconnect":
        streams.stdout((yield* disconnect(command)).id);
        return 0;
      case "session-update":
        streams.stdout(JSON.stringify(yield* updateSessionStatus(command)));
        return 0;
      case "run-create": {
        const created = yield* createRun(command);
        streams.stdout(command.json ? JSON.stringify(created) : created.id);
        return 0;
      }
      case "run-agent-set":
        streams.stdout(JSON.stringify(yield* setRunAgent(command)));
        return 0;
      case "run-list": {
        const runs = runsByRecency((yield* readWorkspace(command.scope)).runs);
        streams.stdout(
          command.json
            ? JSON.stringify(runs)
            : runs
                .map((run) => `${run.id}\t${run.status}\t${run.name}`)
                .join("\n"),
        );
        return 0;
      }
      case "run-show": {
        const run = (yield* readWorkspace(command.scope)).runs.find(
          (candidate) => candidate.id === command.id,
        );
        if (!run)
          return yield* new StateError({
            message: `unknown run: ${command.id}`,
          });
        streams.stdout(
          command.json
            ? JSON.stringify(run)
            : `${run.name}\n${run.goal}\n${run.nodes.length} stages`,
        );
        return 0;
      }
      case "run-update":
        streams.stdout(JSON.stringify(yield* updateRunStatus(command)));
        return 0;
      case "node-upsert":
        streams.stdout(JSON.stringify(yield* upsertNode(command)));
        return 0;
      case "node-update":
        streams.stdout(JSON.stringify(yield* updateNodeStatus(command)));
        return 0;
      case "launch":
        return yield* launch(command);
      case "attach":
        yield* attach(command);
        return 0;
      case "inspect":
        yield* inspectSession(command);
        return 0;
      case "tui": {
        const state = yield* reconcileWorkspace(command.scope);
        const providers = yield* listProviders();
        yield* Effect.tryPromise({
          try: () =>
            openTui(state, {
              attach: (session) =>
                Effect.runPromise(
                  attach({
                    direction: "right",
                    id: session.id,
                    scope: state.scope,
                    tag: "attach",
                  }).pipe(Effect.provide(StateStoreLive)),
                ),
              changes: () =>
                Effect.runPromise(
                  providerOutput({
                    action: "changes",
                    scope: state.scope,
                    version: "orc.provider/v1",
                  }),
                ),
              activity: (session) =>
                Effect.runPromise(
                  providerOutput({
                    action: "activity",
                    scope: state.scope,
                    session,
                    version: "orc.provider/v1",
                  }),
                ),
              providers,
              read: () =>
                Effect.runPromise(
                  reconcileWorkspace(state.scope).pipe(
                    Effect.provide(StateStoreLive),
                  ),
                ),
            }),
          catch: (cause) =>
            new StateError({ message: "open terminal interface", cause }),
        });
        return 0;
      }
    }
  });

export const run = (
  args: ReadonlyArray<string>,
  streams: Streams,
  version: string,
): Effect.Effect<number> =>
  program(args, streams, version).pipe(
    Effect.provide(StateStoreLive),
    Effect.catchTags({
      ArgumentError: (error) =>
        Effect.sync(() => {
          streams.stderr(error.message);
          return 2;
        }),
      StateError: (error) =>
        Effect.sync(() => {
          streams.stderr(error.message);
          return 1;
        }),
    }),
  );
