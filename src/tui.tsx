import { basename } from "node:path";
import {
  createCliRenderer,
  type ScrollBoxRenderable,
  type TabSelectRenderable,
} from "@opentui/core";
import { render, useKeyboard, useRenderer } from "@opentui/solid";
import {
  type Key,
  type Palette,
  resolveTerminalPalette,
  spinnerFrames,
} from "@roshbhatia/ts-utils";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import { ansiToStyledText } from "./ansi.ts";
import type {
  LifecycleStatus,
  Session,
  WorkflowRun,
  WorkspaceState,
} from "./domain.ts";
import { fullKeyHelp, keyHelp, tuiActionFor } from "./keymap.ts";
import type { ProviderInfo } from "./provider.ts";
import { queryFilteredStdout } from "./terminal-output.ts";
import {
  type DetailTab,
  explorerRows,
  type GraphNode,
  graphLevels,
  type MainTab,
  moveGraphSelection,
  providerRows,
  rowDetails,
  type ViewRow,
  type WorkflowView,
  workflowRows,
} from "./tui-model.ts";

const tabs: ReadonlyArray<MainTab> = ["explorer", "workflow", "providers"];
const detailTabs: ReadonlyArray<DetailTab> = ["details", "activity", "changes"];

const titleCase = (value: string): string =>
  `${value.slice(0, 1).toUpperCase()}${value.slice(1)}`;

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

const statusColor = (status: LifecycleStatus, palette: Palette): string => {
  if (status === "done") return palette.success;
  if (status === "failed" || status === "blocked") return palette.danger;
  if (status === "waiting" || status === "queued") return palette.warning;
  if (status === "working") return palette.accent;
  return palette.muted;
};

const optionsFor = (
  rows: ReadonlyArray<ViewRow>,
  frame: number,
): ReadonlyArray<{ name: string; description: string; value: string }> =>
  rows.map((row) => ({
    description: truncate(`${row.subtitle}  ${row.goal}`, 96),
    name: `${"  ".repeat(row.depth)}${statusMark(row.status ?? "disconnected", frame)} ${truncate(row.title, 60)}`,
    value: row.id,
  }));

export interface TuiActions {
  readonly activity: (session: Session) => Promise<string>;
  readonly attach: (session: Session) => Promise<void>;
  readonly changes: () => Promise<string>;
  readonly providers: ReadonlyArray<ProviderInfo>;
  readonly read: () => Promise<WorkspaceState>;
}

interface AppProps {
  readonly actions: TuiActions;
  readonly initialState: WorkspaceState;
  readonly palette: Palette;
}

interface GraphProps {
  readonly frame: number;
  readonly levels: ReadonlyArray<ReadonlyArray<GraphNode>>;
  readonly palette: Palette;
  readonly selected: string | undefined;
  readonly setScroll: (value: ScrollBoxRenderable) => void;
}

const Graph = (props: GraphProps) => (
  <scrollbox ref={props.setScroll} scrollX scrollY style={{ flexGrow: 1 }}>
    <box flexDirection="row" gap={2} padding={1}>
      <For each={props.levels}>
        {(level, index) => (
          <box flexDirection="column" gap={1} width={36}>
            <text fg={props.palette.muted}>{`stage ${index()}`}</text>
            <For each={level}>
              {(node) => (
                <box
                  border
                  borderColor={
                    node.id === props.selected
                      ? props.palette.accent
                      : props.palette.border
                  }
                  height={6}
                  id={node.id}
                  paddingX={1}
                  title={truncate(node.title, 28)}
                >
                  <text fg={statusColor(node.status, props.palette)}>
                    {`${statusMark(node.status, props.frame)} ${truncate(node.subtitle, 30)}\n`}
                    {truncate(node.goal, 62)}
                  </text>
                </box>
              )}
            </For>
          </box>
        )}
      </For>
    </box>
  </scrollbox>
);

const App = (props: AppProps) => {
  const renderer = useRenderer();
  const [state, setState] = createSignal(props.initialState);
  const [tab, setTab] = createSignal<MainTab>("explorer");
  const [detailTab, setDetailTab] = createSignal<DetailTab>("details");
  const [workflowView, setWorkflowView] = createSignal<WorkflowView>("graph");
  const [selectedRun, setSelectedRun] = createSignal(
    state().runs.at(0)?.id ?? "",
  );
  const [selected, setSelected] = createSignal(0);
  const [graphSelected, setGraphSelected] = createSignal<string>();
  const [focus, setFocus] = createSignal<"main" | "details">("main");
  const [frame, setFrame] = createSignal(0);
  const [activity, setActivity] = createSignal<
    Readonly<Record<string, string>>
  >({});
  const [changes, setChanges] = createSignal(
    "Press c to load workspace changes.",
  );
  const [message, setMessage] = createSignal("");
  const [showHelp, setShowHelp] = createSignal(false);
  let mainTabs: TabSelectRenderable | undefined;
  let detailsTabs: TabSelectRenderable | undefined;
  let detailsScroll: ScrollBoxRenderable | undefined;
  let graphScroll: ScrollBoxRenderable | undefined;

  const run = createMemo<WorkflowRun | undefined>(() =>
    state().runs.find((candidate) => candidate.id === selectedRun()),
  );
  const rows = createMemo<ReadonlyArray<ViewRow>>(() => {
    if (tab() === "providers") return providerRows(props.actions.providers);
    if (tab() === "workflow") return workflowRows(state(), run());
    return explorerRows(state());
  });
  const levels = createMemo(() => graphLevels(state(), run()));
  const graphRow = createMemo<ViewRow | undefined>(() => {
    const selectedNode = levels()
      .flat()
      .find((node) => node.id === graphSelected());
    if (!selectedNode) return undefined;
    if (selectedNode.node)
      return workflowRows(state(), run()).find(
        (row) => row.node?.id === selectedNode.node?.id,
      );
    return explorerRows(state()).find(
      (row) => row.session?.id === selectedNode.session?.id,
    );
  });
  const current = createMemo<ViewRow | undefined>(() =>
    tab() === "workflow" && workflowView() === "graph"
      ? graphRow()
      : rows()[Math.min(selected(), Math.max(0, rows().length - 1))],
  );
  const currentSession = createMemo(() => current()?.session);

  createEffect(() => {
    mainTabs?.setSelectedIndex(tabs.indexOf(tab()));
    detailsTabs?.setSelectedIndex(detailTabs.indexOf(detailTab()));
  });
  createEffect(() => {
    const count = rows().length;
    setSelected((value) => Math.min(value, Math.max(0, count - 1)));
  });
  createEffect(() => {
    const first = levels().flat().at(0)?.id;
    if (
      !levels()
        .flat()
        .some((node) => node.id === graphSelected())
    )
      setGraphSelected(first);
  });
  createEffect(() => {
    const id = graphSelected();
    if (id) graphScroll?.scrollChildIntoView(id);
  });

  const refresh = async (): Promise<void> => {
    try {
      setState(await props.actions.read());
    } catch (cause) {
      setMessage(cause instanceof Error ? cause.message : String(cause));
    }
  };

  onMount(() => {
    const refreshTimer = setInterval(() => void refresh(), 1500);
    const spinnerTimer = setInterval(() => setFrame((value) => value + 1), 120);
    onCleanup(() => {
      clearInterval(refreshTimer);
      clearInterval(spinnerTimer);
    });
  });

  const attach = async (): Promise<void> => {
    const session = currentSession();
    if (!session) {
      setMessage("Select a session with an active provider binding.");
      return;
    }
    try {
      await props.actions.attach(session);
      setMessage(`Attached ${session.title}.`);
    } catch (cause) {
      setMessage(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const loadActivity = async (): Promise<void> => {
    const session = currentSession();
    if (!session) {
      setMessage("Select a session.");
      return;
    }
    setDetailTab("activity");
    try {
      const output = await props.actions.activity(session);
      setActivity((value) => ({ ...value, [session.id]: output }));
      setMessage("");
    } catch (cause) {
      setMessage(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const loadChanges = async (): Promise<void> => {
    setDetailTab("changes");
    try {
      setChanges(await props.actions.changes());
      setMessage("");
    } catch (cause) {
      setMessage(cause instanceof Error ? cause.message : String(cause));
    }
  };

  const changeTab = (delta: number): void => {
    setTab(
      (value) =>
        tabs[(tabs.indexOf(value) + delta + tabs.length) % tabs.length] ??
        "explorer",
    );
    setSelected(0);
  };

  const changeDetailTab = (delta: number): void => {
    setDetailTab(
      (value) =>
        detailTabs[
          (detailTabs.indexOf(value) + delta + detailTabs.length) %
            detailTabs.length
        ] ?? "details",
    );
    detailsScroll?.scrollTo(0);
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
    else if (action === "focus-left") setFocus("main");
    else if (action === "focus-right") setFocus("details");
    else if (action === "detail-previous") changeDetailTab(-1);
    else if (action === "detail-next") changeDetailTab(1);
    else if (action === "tab-next") changeTab(1);
    else if (action === "tab-previous") changeTab(-1);
    else if (action === "tree" && tab() === "workflow") setWorkflowView("tree");
    else if (action === "graph" && tab() === "workflow")
      setWorkflowView("graph");
    else if (action === "page-up") detailsScroll?.scrollBy(-8, "step");
    else if (action === "page-down") detailsScroll?.scrollBy(8, "step");
    else if (action === "refresh") void refresh();
    else if (action === "activity") void loadActivity();
    else if (action === "changes") void loadChanges();
    else if (action === "open") {
      const row = current();
      if (row?.run && row.kind === "run") {
        setSelectedRun(row.run.id);
        setTab("workflow");
        setSelected(0);
      } else void attach();
    } else if (
      tab() === "workflow" &&
      workflowView() === "graph" &&
      (action === "next" ||
        action === "previous" ||
        action === "left" ||
        action === "right")
    ) {
      const direction =
        action === "next" ? "down" : action === "previous" ? "up" : action;
      setGraphSelected((value) =>
        moveGraphSelection(levels(), value, direction),
      );
    } else if (focus() === "details" && action === "next")
      detailsScroll?.scrollBy(1, "step");
    else if (focus() === "details" && action === "previous")
      detailsScroll?.scrollBy(-1, "step");
    else if (action === "next" && count > 0)
      setSelected((value) => (value + 1) % count);
    else if (action === "previous" && count > 0)
      setSelected((value) => (value - 1 + count) % count);
  });

  const detailContent = createMemo(() => {
    if (showHelp()) return ["Keys", "", ...fullKeyHelp()].join("\n");
    if (detailTab() === "changes") return ansiToStyledText(changes());
    if (detailTab() === "activity") {
      const session = currentSession();
      return ansiToStyledText(
        session
          ? (activity()[session.id] ?? "Press i to load session activity.")
          : "Select a session.",
      );
    }
    return rowDetails(current());
  });

  return (
    <box
      backgroundColor={props.palette.background}
      flexDirection="column"
      height="100%"
      width="100%"
    >
      <box flexDirection="row" height={1} justifyContent="space-between">
        <text fg={props.palette.title}>
          {`orc  ${basename(state().scope)}  ${state().active ? "active" : "idle"}`}
        </text>
        <text fg={props.palette.muted}>
          {`${state().sessions.length} sessions  ${state().runs.length} workflows`}
        </text>
      </box>
      <tab_select
        backgroundColor="transparent"
        focused={false}
        height={2}
        keyBindings={[]}
        onSelect={(index) => {
          setTab(tabs[index] ?? "explorer");
          setSelected(0);
        }}
        options={tabs.map((value) => ({
          description: "",
          name: titleCase(value),
          value,
        }))}
        ref={(value) => {
          mainTabs = value;
        }}
        selectedBackgroundColor="transparent"
        selectedTextColor={props.palette.accent}
        showDescription={false}
        showUnderline
        textColor={props.palette.muted}
      />
      <box
        border
        borderColor={
          focus() === "main" ? props.palette.accent : props.palette.border
        }
        flexDirection="column"
        flexGrow={1}
        title={
          tab() === "workflow"
            ? `${run()?.name ?? "Workflow"}  ${workflowView() === "graph" ? "[graph] tree" : "graph [tree]"}`
            : titleCase(tab())
        }
      >
        <Show
          fallback={
            <select
              backgroundColor="transparent"
              descriptionColor={props.palette.muted}
              focused={false}
              focusedBackgroundColor="transparent"
              focusedTextColor={props.palette.text}
              itemSpacing={0}
              keyBindings={[]}
              onSelect={(index) => setSelected(index)}
              options={[...optionsFor(rows(), frame())]}
              selectedBackgroundColor="transparent"
              selectedDescriptionColor={props.palette.muted}
              selectedIndex={selected()}
              selectedTextColor={props.palette.accent}
              showScrollIndicator
              textColor={props.palette.text}
              wrapSelection
            />
          }
          when={tab() === "workflow" && workflowView() === "graph"}
        >
          <Graph
            frame={frame()}
            levels={levels()}
            palette={props.palette}
            selected={graphSelected()}
            setScroll={(value) => {
              graphScroll = value;
            }}
          />
        </Show>
      </box>
      <tab_select
        backgroundColor="transparent"
        focused={false}
        height={2}
        keyBindings={[]}
        onSelect={(index) => setDetailTab(detailTabs[index] ?? "details")}
        options={detailTabs.map((value) => ({
          description: "",
          name: titleCase(value),
          value,
        }))}
        ref={(value) => {
          detailsTabs = value;
        }}
        selectedBackgroundColor="transparent"
        selectedTextColor={props.palette.accent}
        showDescription={false}
        showUnderline
        textColor={props.palette.muted}
      />
      <scrollbox
        border
        borderColor={
          focus() === "details" ? props.palette.accent : props.palette.border
        }
        focused={false}
        height="34%"
        padding={1}
        ref={(value) => {
          detailsScroll = value;
        }}
        scrollX
        scrollY
      >
        <text content={detailContent()} fg={props.palette.text} />
      </scrollbox>
      <box flexDirection="row" height={1} justifyContent="space-between">
        <text fg={message() ? props.palette.warning : props.palette.muted}>
          {message() ||
            keyHelp([
              "next",
              "previous",
              "left",
              "right",
              "open",
              "activity",
              "changes",
              "tree",
              "graph",
              "tab-next",
              "help",
              "quit",
            ])}
        </text>
        <text fg={props.palette.muted}>{focus()}</text>
      </box>
    </box>
  );
};

export const openTui = async (
  state: WorkspaceState,
  actions: TuiActions,
): Promise<void> => {
  const renderer = await createCliRenderer({
    autoFocus: false,
    backgroundColor: "transparent",
    exitOnCtrlC: true,
    remote: false,
    stdout: queryFilteredStdout(),
    targetFps: 30,
  });
  const resize = (): void =>
    renderer.resize(process.stdout.columns, process.stdout.rows);
  process.on("SIGWINCH", resize);
  renderer.once("destroy", () => process.off("SIGWINCH", resize));
  const palette = resolveTerminalPalette();
  const destroyed = new Promise<void>((resolve) =>
    renderer.once("destroy", resolve),
  );
  await render(
    () => <App actions={actions} initialState={state} palette={palette} />,
    renderer,
  );
  await destroyed;
};
