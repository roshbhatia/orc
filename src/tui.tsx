import { basename } from "node:path";
import { createCliRenderer } from "@opentui/core";
import { render, useKeyboard, useRenderer } from "@opentui/solid";
import { defaultPalette } from "@roshbhatia/ts-utils";
import { createMemo, createSignal } from "solid-js";
import type { Session, WorkspaceState } from "./domain.ts";

type Tab = "explorer" | "runs";

const statusMark = (session: Session): string => {
  switch (session.status) {
    case "working":
      return "*";
    case "waiting":
      return "o";
    case "blocked":
      return "!";
    case "failed":
      return "x";
    case "done":
      return "+";
    case "disconnected":
      return "-";
  }
};

const truncate = (value: string, width: number): string =>
  value.length <= width ? value : `${value.slice(0, Math.max(0, width - 1))}…`;

const orderedTree = (state: WorkspaceState): ReadonlyArray<Session> => {
  const ids = new Set(state.sessions.map((session) => session.id));
  const recent = [...state.sessions].sort((left, right) =>
    right.updatedAt.localeCompare(left.updatedAt),
  );
  const roots = recent.filter(
    (session) => !session.parentId || !ids.has(session.parentId),
  );
  const ordered: Array<Session> = [];
  const seen = new Set<string>();
  const visit = (session: Session): void => {
    if (seen.has(session.id)) {
      return;
    }
    seen.add(session.id);
    ordered.push(session);
    for (const child of recent.filter(
      (candidate) => candidate.parentId === session.id,
    )) {
      visit(child);
    }
  };
  for (const root of roots) {
    visit(root);
  }
  for (const session of recent) {
    visit(session);
  }
  return ordered;
};

const depthOf = (
  session: Session,
  sessions: ReadonlyArray<Session>,
): number => {
  const byId = new Map(sessions.map((candidate) => [candidate.id, candidate]));
  const seen = new Set<string>();
  let depth = 0;
  let parentId = session.parentId;
  while (parentId && !seen.has(parentId)) {
    seen.add(parentId);
    const parent = byId.get(parentId);
    if (!parent) {
      break;
    }
    depth++;
    parentId = parent.parentId;
  }
  return depth;
};

const explorerText = (
  state: WorkspaceState,
  sessions: ReadonlyArray<Session>,
  selected: number,
): string => {
  if (sessions.length === 0) {
    return [
      "No sessions are registered.",
      "",
      "Run orc connect from an agent session.",
    ].join("\n");
  }
  return sessions
    .map((session, index) => {
      const depth = depthOf(session, state.sessions);
      const cursor = index === selected ? ">" : " ";
      const branch = depth > 0 ? `${"  ".repeat(depth - 1)}+- ` : "";
      return [
        `${cursor} ${branch}${statusMark(session)} ${truncate(session.purpose, 42)}`,
        `  ${"  ".repeat(depth)}${session.role} · ${session.harness} · ${session.status}`,
        `  ${"  ".repeat(depth)}goal: ${truncate(session.goal, 64)}`,
      ].join("\n");
    })
    .join("\n\n");
};

const runsText = (
  sessions: ReadonlyArray<Session>,
  selected: number,
): string => {
  if (sessions.length === 0) {
    return "No active pipeline nodes.";
  }
  return sessions
    .map((session, index) => {
      const cursor = index === selected ? ">" : " ";
      return [
        `${cursor} ${statusMark(session)} ${truncate(session.goal, 64)}`,
        `  ${session.role} · returns to ${session.completion}`,
      ].join("\n");
    })
    .join("\n\n");
};

const detailsText = (session: Session | undefined): string => {
  if (!session) {
    return "Select a session to inspect its contract.";
  }
  return [
    session.purpose,
    "",
    `role             ${session.role}`,
    `harness          ${session.harness}`,
    `status           ${session.status}`,
    `goal             ${session.goal}`,
    `expected output  ${session.expectedOutput}`,
    `completion       ${session.completion}`,
    `native session   ${session.nativeId}`,
    `orc session      ${session.id}`,
    `parent           ${session.parentId ?? "root"}`,
    `zmx              ${session.zmxSession ?? "unavailable"}`,
  ].join("\n");
};

export interface TuiActions {
  readonly attach: (session: Session) => Promise<void>;
  readonly traces: (session: Session) => Promise<void>;
}

interface AppProps {
  readonly state: WorkspaceState;
  readonly actions: TuiActions;
}

const App = (props: AppProps) => {
  const renderer = useRenderer();
  const sessions = createMemo(() => orderedTree(props.state));
  const [tab, setTab] = createSignal<Tab>("explorer");
  const [selected, setSelected] = createSignal(0);
  const [footer, setFooter] = createSignal(
    "j/k select  enter attach  t traces  tab view  q quit",
  );
  const current = createMemo(() => sessions()[selected()]);

  const perform = async (
    label: string,
    action: (session: Session) => Promise<void>,
  ): Promise<void> => {
    const session = current();
    if (!session) {
      return;
    }
    setFooter(`${label} ${session.purpose}`);
    try {
      await action(session);
      setFooter(`${label} opened for ${session.purpose}`);
    } catch (cause) {
      setFooter(cause instanceof Error ? cause.message : String(cause));
    }
  };

  useKeyboard((key) => {
    const count = sessions().length;
    if (key.name === "q" || key.name === "escape") {
      renderer.destroy();
    } else if (key.name === "tab") {
      setTab((value) => (value === "explorer" ? "runs" : "explorer"));
    } else if (key.name === "j" || key.name === "down") {
      if (count > 0) {
        setSelected((value) => (value + 1) % count);
      }
    } else if (key.name === "k" || key.name === "up") {
      if (count > 0) {
        setSelected((value) => (value - 1 + count) % count);
      }
    } else if (key.name === "return") {
      void perform("attach", props.actions.attach);
    } else if (key.name === "t") {
      void perform("traces", props.actions.traces);
    }
  });

  return (
    <box
      backgroundColor={defaultPalette.background}
      flexDirection="column"
      height="100%"
      width="100%"
    >
      <text fg={defaultPalette.accent} height={1}>
        {`orc  ${basename(props.state.scope)}  ${props.state.sessions.length} sessions  ${props.state.active ? "active" : "idle"}`}
      </text>
      <text fg={defaultPalette.text} height={1}>
        {tab() === "explorer" ? "[explorer]  runs" : " explorer  [runs]"}
      </text>
      <box flexDirection="row" flexGrow={1} gap={1}>
        <box
          border
          borderColor={defaultPalette.border}
          flexGrow={1}
          padding={1}
          title={tab()}
        >
          <text fg={defaultPalette.text}>
            {tab() === "explorer"
              ? explorerText(props.state, sessions(), selected())
              : runsText(sessions(), selected())}
          </text>
        </box>
        <box
          border
          borderColor={defaultPalette.border}
          padding={1}
          title="details"
          width="38%"
        >
          <text fg={defaultPalette.text}>{detailsText(current())}</text>
        </box>
      </box>
      <text fg={defaultPalette.muted} height={1}>
        {footer()}
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
  await render(() => <App actions={actions} state={state} />, renderer);
  await destroyed;
};
