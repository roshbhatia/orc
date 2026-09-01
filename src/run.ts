import { Effect } from "effect";
import { parseArgs } from "./args.ts";
import {
  attach,
  connect,
  createRun,
  currentSession,
  disconnect,
  launch,
  openTraces,
  readWorkspace,
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
import { StateError, StateStoreLive } from "./state.ts";
import { openTui } from "./tui.tsx";

export interface Streams {
  readonly stdout: (value: string) => void;
  readonly stderr: (value: string) => void;
}

export const help = `orc: local control plane for agent harnesses

usage:
  orc
  orc status|list [--scope <path>] [--json]
  orc connect [contract options]
  orc session register [contract options] [--hook-input] [--quiet]
  orc session current|list [--json]
  orc session update <id> --status <status>
  orc run create|list|show|update
  orc node upsert|update
  orc launch <harness> [--zmx <name>] -- [args]
  orc attach|traces <session-id> [--direction <direction>]
  orc disconnect [session-id]
  orc mcp

contract options:
  --harness <name>  --role <role>  --title <name>
  --purpose <reason>  --goal <goal>  --expected-output <contract>
  --success <criterion>  --completion <orchestrator|judge>
  --review-by <node-id>  --parent <session-id>

Orc activates with the first registered session and idles after the last disconnect.`;

const displayList = (state: WorkspaceState): string =>
  sessionsByRecency(state.sessions)
    .map(
      (session) =>
        `${session.id}\t${session.status}\t${session.role}\t${session.title}`,
    )
    .join("\n");

const changesOutput = async (scope: string): Promise<string> => {
  const child = Bun.spawn(
    ["changes", "-r", "-root", scope, "-color", "always"],
    {
      cwd: scope,
      stderr: "pipe",
      stdout: "pipe",
    },
  );
  const stdout = await new Response(child.stdout).text();
  const stderr = await new Response(child.stderr).text();
  const code = await child.exited;
  if (code !== 0)
    throw new Error(stderr.trim() || `changes exited with code ${code}`);
  return stdout.trim() || "No workspace changes.";
};

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
      case "run-create":
        streams.stdout(JSON.stringify(yield* createRun(command)));
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
      case "traces":
        yield* openTraces(command);
        return 0;
      case "tui": {
        const state = yield* readWorkspace(command.scope);
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
              changes: () => changesOutput(state.scope),
              read: () =>
                Effect.runPromise(
                  readWorkspace(state.scope).pipe(
                    Effect.provide(StateStoreLive),
                  ),
                ),
              traces: (session) =>
                Effect.runPromise(
                  openTraces({
                    direction: "right",
                    id: session.id,
                    scope: state.scope,
                    tag: "traces",
                  }).pipe(Effect.provide(StateStoreLive)),
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
