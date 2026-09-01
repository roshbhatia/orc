import { basename } from "node:path";
import { createCliRenderer } from "@opentui/core";
import { render, useKeyboard, useRenderer } from "@opentui/solid";
import { defaultPalette, type Key, spinnerFrames } from "@roshbhatia/ts-utils";
import { createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type {
  LifecycleStatus,
  Session,
  WorkflowRun,
  WorkspaceState,
} from "./domain.ts";
import { fullKeyHelp, keyHelp, tuiActionFor } from "./keymap.ts";

type Tab = "explorer" | "runs" | "sessions";
type DetailTab = "details" | "session" | "changes";

interface Row {
  readonly id: string;
  readonly kind: "run" | "node" | "session";
  readonly depth: number;
  readonly title: string;
  readonly subtitle: string;
  readonly goal: string;
  readonly status: LifecycleStatus;
  readonly session?: Session;
  readonly run?: WorkflowRun;
}

const truncate = (value: string, width: number): string =>
  value.length <= width ? value : `${value.slice(0, Math.max(0, width - 1))}…`;

const statusMark = (status: LifecycleStatus, frame: number): string => {
  if (status === "working")
    return spinnerFrames[frame % spinnerFrames.length] ?? "|";
  if (status === "done") return "+";
  if (status === "failed") return "x";
  if (status === "blocked") return "!";
  if (status === "waiting" || status === "queued") return "o";
  return "-";
};

const sessionRows = (state: WorkspaceState): ReadonlyArray<Row> =>
  [...state.sessions]
    .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt))
    .map((session) => ({
      depth: 0,
      goal: session.goal,
      id: session.id,
      kind: "session",
      session,
      status: session.status,
      subtitle: `${session.role} · ${session.harness}${session.model ? ` · ${session.model}` : ""}`,
      title: session.title,
    }));

const runRows = (state: WorkspaceState): ReadonlyArray<Row> =>
  [...state.runs]
    .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt))
    .flatMap((run) => [
      {
        depth: 0,
        goal: run.goal,
        id: run.id,
        kind: "run" as const,
        run,
        status: run.status,
        subtitle: `${run.nodes.length} stages`,
        title: run.name,
      },
      ...run.nodes.map((node) => ({
        depth: 1,
        goal: node.goal,
        id: `${run.id}:${node.id}`,
        kind: "node" as const,
        run,
        ...(state.sessions.find((session) => session.id === node.sessionId)
          ? {
              session: state.sessions.find(
                (session) => session.id === node.sessionId,
              ) as Session,
            }
          : {}),
        status: node.status,
        subtitle: `${node.role} · ${node.harness}${node.model ? ` · ${node.model}` : ""}`,
        title: node.name,
      })),
    ]);

const explorerRows = (state: WorkspaceState): ReadonlyArray<Row> => {
  const sessions = sessionRows(state);
  const orchestrators = sessions.filter(
    (row) => row.session?.role === "orchestrator",
  );
  const children = sessions.filter(
    (row) => row.session?.role !== "orchestrator",
  );
  const rows: Array<Row> = [];
  for (const root of orchestrators) {
    rows.push(root);
    for (const run of runRows(state).filter(
      (row) => row.kind === "run" && row.run?.orchestratorId === root.id,
    )) {
      rows.push({ ...run, depth: 1 });
      for (const node of runRows(state).filter(
        (row) => row.kind === "node" && row.run?.id === run.run?.id,
      )) {
        rows.push({ ...node, depth: 2 });
      }
    }
    for (const child of children.filter(
      (row) => row.session?.parentId === root.id && !row.session?.runId,
    )) {
      rows.push({ ...child, depth: 1 });
    }
  }
  for (const row of sessions) {
    if (
      !rows.some((candidate) => candidate.id === row.id) &&
      !row.session?.runId
    )
      rows.push(row);
  }
  return rows;
};

const rowsFor = (state: WorkspaceState, tab: Tab): ReadonlyArray<Row> =>
  tab === "runs"
    ? runRows(state)
    : tab === "sessions"
      ? sessionRows(state)
      : explorerRows(state);

const listText = (
  rows: ReadonlyArray<Row>,
  selected: number,
  frame: number,
): string =>
  rows.length === 0
    ? "No records."
    : rows
        .map((row, index) => {
          const indent = "  ".repeat(row.depth);
          const branch = row.depth > 0 ? "└─ " : "";
          const cursor = index === selected ? ">" : " ";
          return [
            `${cursor} ${indent}${branch}${statusMark(row.status, frame)} ${truncate(row.title, 66)}  ${row.status}`,
            `  ${indent}   ${truncate(row.subtitle, 54)}  ${truncate(row.goal, 76)}`,
          ].join("\n");
        })
        .join("\n");

const details = (row: Row | undefined): string => {
  if (!row) return "Select a record.";
  if (row.kind === "run" && row.run) {
    return [
      row.run.name,
      "",
      `status           ${row.run.status}`,
      `goal             ${row.run.goal}`,
      `expected output  ${row.run.expectedOutput}`,
      `stages           ${row.run.nodes.length}`,
      "",
      "agent defaults",
      ...row.run.agents.map(
        (agent) =>
          `${agent.role.padEnd(16)} ${agent.harness}${agent.model ? ` · ${agent.model}` : ""}`,
      ),
      `run              ${row.run.id}`,
    ].join("\n");
  }
  if (row.kind === "node" && row.run) {
    const node = row.run.nodes.find(
      (candidate) => `${row.run?.id}:${candidate.id}` === row.id,
    );
    if (node)
      return [
        node.name,
        "",
        `role             ${node.role}`,
        `harness          ${node.harness}`,
        `model            ${node.model ?? "default"}`,
        `status           ${node.status}`,
        `purpose          ${node.purpose}`,
        `goal             ${node.goal}`,
        `expected output  ${node.expectedOutput}`,
        `success          ${node.successCriteria.join("; ") || "unspecified"}`,
        `completion       ${node.completion}`,
        `review by        ${node.reviewBy ?? "orchestrator"}`,
        `session          ${node.sessionId ?? "unassigned"}`,
      ].join("\n");
  }
  const session = row.session;
  return session
    ? [
        session.title,
        "",
        `role             ${session.role}`,
        `harness          ${session.harness}`,
        `model            ${session.model ?? "default"}`,
        `status           ${session.status}`,
        `purpose          ${session.purpose}`,
        `goal             ${session.goal}`,
        `expected output  ${session.expectedOutput}`,
        `success          ${session.successCriteria.join("; ") || "unspecified"}`,
        `completion       ${session.completion}`,
        `native session   ${session.nativeId}`,
        `orc session      ${session.id}`,
        `parent           ${session.parentId ?? "root"}`,
        `zmx              ${session.zmxSession ?? "unavailable"}`,
      ].join("\n")
    : "Select a record.";
};

export interface TuiActions {
  readonly read: () => Promise<WorkspaceState>;
  readonly attach: (session: Session) => Promise<void>;
  readonly traces: (session: Session) => Promise<void>;
  readonly changes: () => Promise<string>;
}

interface AppProps {
  readonly initialState: WorkspaceState;
  readonly actions: TuiActions;
}

const tabs: ReadonlyArray<Tab> = ["explorer", "runs", "sessions"];
const detailTabs: ReadonlyArray<DetailTab> = ["details", "session", "changes"];

const App = (props: AppProps) => {
  const renderer = useRenderer();
  const [state, setState] = createSignal(props.initialState);
  const [tab, setTab] = createSignal<Tab>("explorer");
  const [detailTab, setDetailTab] = createSignal<DetailTab>("details");
  const [selected, setSelected] = createSignal(0);
  const [frame, setFrame] = createSignal(0);
  const [scroll, setScroll] = createSignal(0);
  const [changes, setChanges] = createSignal(
    "Load workspace changes from the changes action.",
  );
  const [message, setMessage] = createSignal("");
  const [showHelp, setShowHelp] = createSignal(false);
  const rows = createMemo(() => rowsFor(state(), tab()));
  const current = createMemo(
    () => rows()[Math.min(selected(), Math.max(0, rows().length - 1))],
  );

  const refresh = async (): Promise<void> => {
    try {
      setState(await props.actions.read());
      setSelected((value) => Math.min(value, Math.max(0, rows().length - 1)));
    } catch (cause) {
      setMessage(cause instanceof Error ? cause.message : String(cause));
    }
  };

  onMount(() => {
    const refreshTimer = setInterval(() => void refresh(), 1000);
    const spinnerTimer = setInterval(() => setFrame((value) => value + 1), 120);
    onCleanup(() => {
      clearInterval(refreshTimer);
      clearInterval(spinnerTimer);
    });
  });

  const perform = async (
    name: string,
    action: (session: Session) => Promise<void>,
  ): Promise<void> => {
    const session = current()?.session;
    if (!session) {
      setMessage("Select a session.");
      return;
    }
    try {
      await action(session);
      setMessage(`${name}: ${session.title}`);
    } catch (cause) {
      setMessage(cause instanceof Error ? cause.message : String(cause));
    }
  };

  useKeyboard((key) => {
    const action = tuiActionFor(key as Key);
    const count = rows().length;
    if (showHelp() && action !== "help" && action !== "quit") {
      setShowHelp(false);
      return;
    }
    if (action === "quit") renderer.destroy();
    else if (action === "help") setShowHelp((value) => !value);
    else if (action === "next" && count > 0)
      setSelected((value) => (value + 1) % count);
    else if (action === "previous" && count > 0)
      setSelected((value) => (value - 1 + count) % count);
    else if (action === "tab-next" || action === "tab-previous") {
      const delta = action === "tab-next" ? 1 : -1;
      setTab(
        (value) =>
          tabs[(tabs.indexOf(value) + delta + tabs.length) % tabs.length] ??
          "explorer",
      );
      setSelected(0);
    } else if (action === "left" || action === "right") {
      const delta = action === "right" ? 1 : -1;
      setDetailTab(
        (value) =>
          detailTabs[
            (detailTabs.indexOf(value) + delta + detailTabs.length) %
              detailTabs.length
          ] ?? "details",
      );
      setScroll(0);
    } else if (action === "page-up")
      setScroll((value) => Math.max(0, value - 8));
    else if (action === "page-down") setScroll((value) => value + 8);
    else if (action === "refresh") void refresh();
    else if (action === "open") {
      const row = current();
      if (row?.kind === "run") setTab("runs");
      else void perform("attach", props.actions.attach);
    } else if (action === "traces")
      void perform("traces", props.actions.traces);
    else if (action === "changes") {
      setDetailTab("changes");
      void props.actions
        .changes()
        .then(setChanges)
        .catch((cause: unknown) =>
          setMessage(cause instanceof Error ? cause.message : String(cause)),
        );
    }
  });

  const detailText = createMemo(() => {
    if (showHelp()) return ["Keys", "", ...fullKeyHelp()].join("\n");
    if (detailTab() === "changes") return changes();
    if (detailTab() === "session") {
      const session = current()?.session;
      return session
        ? [
            `${session.title} · ${session.status}`,
            "",
            session.goal,
            "",
            `Open Traces for the full transcript.`,
            `Attach through ZMX to interact.`,
          ].join("\n")
        : "Select a session.";
    }
    return details(current());
  });

  return (
    <box
      backgroundColor={defaultPalette.background}
      flexDirection="column"
      height="100%"
      width="100%"
    >
      <text
        fg={defaultPalette.accent}
        height={1}
      >{`orc  ${basename(state().scope)}  ${state().active ? "active" : "idle"}  ${state().sessions.length} sessions`}</text>
      <text fg={defaultPalette.text} height={1}>
        {tabs
          .map((value) => (value === tab() ? `[${value}]` : value))
          .join("  ")}
      </text>
      <box
        border
        borderColor={defaultPalette.border}
        flexGrow={1}
        padding={1}
        title={tab()}
      >
        <text fg={defaultPalette.text}>
          {listText(rows(), selected(), frame())}
        </text>
      </box>
      <box
        border
        borderColor={defaultPalette.border}
        height="36%"
        padding={1}
        title={detailTabs
          .map((value) => (value === detailTab() ? `[${value}]` : value))
          .join("  ")}
      >
        <text fg={defaultPalette.text}>
          {detailText().split("\n").slice(scroll()).join("\n")}
        </text>
      </box>
      <text
        fg={message() ? defaultPalette.warning : defaultPalette.muted}
        height={1}
      >
        {message() ||
          keyHelp([
            "next",
            "previous",
            "open",
            "traces",
            "changes",
            "tab-next",
            "help",
            "quit",
          ])}
      </text>
    </box>
  );
};

export const openTui = async (
  state: WorkspaceState,
  actions: TuiActions,
): Promise<void> => {
  const renderer = await createCliRenderer({
    backgroundColor: defaultPalette.background,
    exitOnCtrlC: true,
    targetFps: 30,
  });
  const destroyed = new Promise<void>((resolve) =>
    renderer.once("destroy", resolve),
  );
  await render(() => <App actions={actions} initialState={state} />, renderer);
  await destroyed;
};
