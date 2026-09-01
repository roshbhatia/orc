import type {
  LifecycleStatus,
  Session,
  WorkflowNode,
  WorkflowRun,
  WorkspaceState,
} from "./domain.ts";
import type { ProviderInfo } from "./provider.ts";

export type MainTab = "explorer" | "workflow" | "providers";
export type DetailTab = "details" | "activity" | "changes";
export type WorkflowView = "tree" | "graph";

export interface ViewRow {
  readonly depth: number;
  readonly goal: string;
  readonly id: string;
  readonly kind: "run" | "node" | "session" | "provider";
  readonly node?: WorkflowNode;
  readonly provider?: ProviderInfo;
  readonly run?: WorkflowRun;
  readonly session?: Session;
  readonly status?: LifecycleStatus;
  readonly subtitle: string;
  readonly title: string;
}

export interface GraphNode {
  readonly goal: string;
  readonly id: string;
  readonly level: number;
  readonly node?: WorkflowNode;
  readonly session?: Session;
  readonly status: LifecycleStatus;
  readonly subtitle: string;
  readonly title: string;
}

const byRecency = <Item extends { readonly updatedAt: string }>(
  items: ReadonlyArray<Item>,
): ReadonlyArray<Item> =>
  [...items].sort((left, right) =>
    right.updatedAt.localeCompare(left.updatedAt),
  );

const sessionRow = (session: Session, depth = 0): ViewRow => ({
  depth,
  goal: session.goal,
  id: session.id,
  kind: "session",
  session,
  status: session.status,
  subtitle: `${session.role} · ${session.harness}${session.model ? ` · ${session.model}` : ""}`,
  title: session.title,
});

const runRow = (run: WorkflowRun, depth = 0): ViewRow => ({
  depth,
  goal: run.goal,
  id: run.id,
  kind: "run",
  run,
  status: run.status,
  subtitle: `${run.nodes.length} ${run.nodes.length === 1 ? "stage" : "stages"}`,
  title: run.name,
});

const nodeRow = (
  run: WorkflowRun,
  node: WorkflowNode,
  state: WorkspaceState,
  depth = 0,
): ViewRow => {
  const session = state.sessions.find(
    (candidate) => candidate.id === node.sessionId,
  );
  return {
    depth,
    goal: node.goal,
    id: `${run.id}:${node.id}`,
    kind: "node",
    node,
    run,
    ...(session ? { session } : {}),
    status: node.status,
    subtitle: `${node.role} · ${node.harness}${node.model ? ` · ${node.model}` : ""}`,
    title: node.name,
  };
};

export const explorerRows = (state: WorkspaceState): ReadonlyArray<ViewRow> => {
  const rows: ViewRow[] = [];
  const included = new Set<string>();
  const orchestrators = byRecency(state.sessions).filter(
    (session) => session.role === "orchestrator",
  );
  for (const orchestrator of orchestrators) {
    rows.push(sessionRow(orchestrator));
    included.add(orchestrator.id);
    for (const run of byRecency(state.runs).filter(
      (candidate) => candidate.orchestratorId === orchestrator.id,
    )) {
      rows.push(runRow(run, 1));
      for (const node of run.nodes) {
        rows.push(nodeRow(run, node, state, 2));
        if (node.sessionId) included.add(node.sessionId);
      }
    }
    for (const child of byRecency(state.sessions).filter(
      (session) =>
        session.parentId === orchestrator.id &&
        !session.runId &&
        !included.has(session.id),
    )) {
      rows.push(sessionRow(child, 1));
      included.add(child.id);
    }
  }
  for (const session of byRecency(state.sessions)) {
    if (!included.has(session.id)) rows.push(sessionRow(session));
  }
  for (const run of byRecency(state.runs)) {
    if (!rows.some((row) => row.kind === "run" && row.id === run.id)) {
      rows.push(runRow(run));
      for (const node of run.nodes) rows.push(nodeRow(run, node, state, 1));
    }
  }
  return rows;
};

export const workflowRows = (
  state: WorkspaceState,
  run: WorkflowRun | undefined,
): ReadonlyArray<ViewRow> =>
  run
    ? [runRow(run), ...run.nodes.map((node) => nodeRow(run, node, state, 1))]
    : [];

export const providerRows = (
  providers: ReadonlyArray<ProviderInfo>,
): ReadonlyArray<ViewRow> =>
  providers.map((provider) => ({
    depth: 0,
    goal: provider.capabilities.join(", "),
    id: provider.name,
    kind: "provider",
    provider,
    subtitle: `${provider.kind} · priority ${provider.priority}`,
    title: provider.name,
  }));

const nodeLevel = (
  node: WorkflowNode,
  run: WorkflowRun,
  levels: ReadonlyMap<string, number>,
): number => {
  const dependencies = run.edges
    .filter((edge) => edge.to === node.id)
    .map((edge) => levels.get(edge.from) ?? 0);
  return dependencies.length === 0 ? 1 : Math.max(...dependencies) + 1;
};

export const graphLevels = (
  state: WorkspaceState,
  run: WorkflowRun | undefined,
): ReadonlyArray<ReadonlyArray<GraphNode>> => {
  if (!run) return [];
  const root = state.sessions.find(
    (session) => session.id === run.orchestratorId,
  );
  const levels = new Map<string, number>();
  const remaining = [...run.nodes];
  for (let pass = 0; pass <= run.nodes.length && remaining.length > 0; pass++) {
    for (let index = remaining.length - 1; index >= 0; index--) {
      const node = remaining[index];
      if (!node) continue;
      const dependencies = run.edges
        .filter((edge) => edge.to === node.id)
        .map((edge) => edge.from);
      if (dependencies.some((id) => !levels.has(id))) continue;
      levels.set(node.id, nodeLevel(node, run, levels));
      remaining.splice(index, 1);
    }
  }
  for (const node of remaining) levels.set(node.id, 1);
  const graph: GraphNode[] = [
    {
      goal: run.goal,
      id: `orchestrator:${run.id}`,
      level: 0,
      ...(root ? { session: root } : {}),
      status: root?.status ?? run.status,
      subtitle: root
        ? `${root.role} · ${root.harness}${root.model ? ` · ${root.model}` : ""}`
        : "orchestrator · unassigned",
      title: root?.title ?? run.name,
    },
    ...run.nodes.map((node) => {
      const session = state.sessions.find(
        (candidate) => candidate.id === node.sessionId,
      );
      return {
        goal: node.goal,
        id: `node:${node.id}`,
        level: levels.get(node.id) ?? 1,
        node,
        ...(session ? { session } : {}),
        status: node.status,
        subtitle: `${node.role} · ${node.harness}${node.model ? ` · ${node.model}` : ""}`,
        title: node.name,
      };
    }),
  ];
  const highest = Math.max(...graph.map((node) => node.level));
  return Array.from({ length: highest + 1 }, (_, level) =>
    graph.filter((node) => node.level === level),
  );
};

export const moveGraphSelection = (
  levels: ReadonlyArray<ReadonlyArray<GraphNode>>,
  selected: string | undefined,
  direction: "up" | "down" | "left" | "right",
): string | undefined => {
  const flat = levels.flat();
  const current = flat.find((node) => node.id === selected) ?? flat[0];
  if (!current) return undefined;
  const level = levels[current.level] ?? [];
  const row = Math.max(
    0,
    level.findIndex((node) => node.id === current.id),
  );
  if (direction === "up" || direction === "down") {
    const delta = direction === "down" ? 1 : -1;
    return level[(row + delta + level.length) % level.length]?.id ?? current.id;
  }
  const delta = direction === "right" ? 1 : -1;
  const target = levels[current.level + delta];
  return target?.[Math.min(row, target.length - 1)]?.id ?? current.id;
};

export const rowDetails = (row: ViewRow | undefined): string => {
  if (!row) return "Select an item.";
  if (row.provider)
    return [
      row.provider.name,
      "",
      `kind          ${row.provider.kind}`,
      `priority      ${row.provider.priority}`,
      `capabilities  ${row.provider.capabilities.join(", ")}`,
      `command       ${row.provider.command}`,
    ].join("\n");
  if (row.node)
    return [
      row.node.name,
      "",
      `role             ${row.node.role}`,
      `harness          ${row.node.harness}`,
      `model            ${row.node.model ?? "default"}`,
      `status           ${row.node.status}`,
      `purpose          ${row.node.purpose}`,
      `goal             ${row.node.goal}`,
      `expected output  ${row.node.expectedOutput}`,
      `success          ${row.node.successCriteria.join("; ") || "unspecified"}`,
      `completion       ${row.node.completion}`,
      `review by        ${row.node.reviewBy ?? "orchestrator"}`,
      `session          ${row.node.sessionId ?? "unassigned"}`,
    ].join("\n");
  if (row.kind === "run" && row.run)
    return [
      row.run.name,
      "",
      `status           ${row.run.status}`,
      `goal             ${row.run.goal}`,
      `expected output  ${row.run.expectedOutput}`,
      `stages           ${row.run.nodes.length}`,
      `run              ${row.run.id}`,
    ].join("\n");
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
        "",
        "providers",
        ...(session.providers.length > 0
          ? session.providers.map(
              (binding) =>
                `${binding.kind.padEnd(16)} ${binding.provider} · ${binding.status} · ${binding.label}`,
            )
          : ["none"]),
      ].join("\n")
    : "Select an item.";
};
