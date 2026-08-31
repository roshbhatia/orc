import { Effect } from "effect";
import { parseArgs } from "./args.ts";
import {
  attach,
  connect,
  disconnect,
  openTraces,
  readWorkspace,
} from "./control.ts";
import {
  activeSessions,
  sessionsByRecency,
  type WorkspaceState,
} from "./domain.ts";
import { StateError, StateStoreLive } from "./state.ts";
import { openTui } from "./tui.tsx";

export interface Streams {
  readonly stdout: (value: string) => void;
  readonly stderr: (value: string) => void;
}

export const help = `orc: local control plane for agent harnesses

usage:
  orc
  orc status [--scope <path>] [--json]
  orc list [--scope <path>] [--json]
  orc connect [options]
  orc disconnect [session-id] [--scope <path>]
  orc attach <session-id> [--direction <direction>]
  orc traces <session-id> [--direction <direction>]

connect options:
  --harness <name>
  --role <orchestrator|planner|researcher|implementer|judge|worker>
  --purpose <name>
  --goal <goal>
  --expected-output <contract>
  --completion <orchestrator|judge>
  --parent <session-id>
  --native-id <harness-session-id>
  --zmx <session-name>

Orc starts when a session connects. It becomes idle after the last disconnect.
The default TUI orders each tree level by recent activity.`;

const displayList = (state: WorkspaceState): string =>
  sessionsByRecency(state.sessions)
    .map(
      (session) =>
        `${session.id}\t${session.status}\t${session.role}\t${session.purpose}`,
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
      case "status": {
        const state = yield* readWorkspace(command.scope);
        const result = {
          active: state.active,
          scope: state.scope,
          sessions: state.sessions.length,
          working: activeSessions(state).length,
        };
        streams.stdout(
          command.json
            ? JSON.stringify(result)
            : `${result.active ? "active" : "idle"} · ${result.working} working · ${result.sessions} sessions · ${result.scope}`,
        );
        return 0;
      }
      case "list": {
        const state = yield* readWorkspace(command.scope);
        streams.stdout(
          command.json ? JSON.stringify(state.sessions) : displayList(state),
        );
        return 0;
      }
      case "connect": {
        const session = yield* connect(command);
        streams.stdout(session.id);
        return 0;
      }
      case "disconnect": {
        const session = yield* disconnect(command);
        streams.stdout(session.id);
        return 0;
      }
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
