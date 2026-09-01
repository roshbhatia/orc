import { Effect } from "effect";
import {
  createRun,
  currentSession,
  readWorkspace,
  registerSession,
  setRunAgent,
  updateNodeStatus,
  updateRunStatus,
  updateSessionStatus,
  upsertNode,
} from "./control.ts";
import type { LifecycleStatus, SessionRole } from "./domain.ts";
import { StateStoreLive } from "./state.ts";

interface JsonRpcRequest {
  readonly jsonrpc: "2.0";
  readonly id?: string | number | null;
  readonly method: string;
  readonly params?: unknown;
}

interface ToolDefinition {
  readonly name: string;
  readonly description: string;
  readonly inputSchema: Readonly<Record<string, unknown>>;
}

const objectSchema = (
  properties: Readonly<Record<string, unknown>>,
  required: ReadonlyArray<string> = [],
): Readonly<Record<string, unknown>> => ({
  additionalProperties: false,
  properties,
  required,
  type: "object",
});

const tools: ReadonlyArray<ToolDefinition> = [
  {
    name: "orc_current_session",
    description: "Return the registered Orc session for this harness process.",
    inputSchema: objectSchema({}),
  },
  {
    name: "orc_session_list",
    description: "List sessions in the current Orc scope.",
    inputSchema: objectSchema({}),
  },
  {
    name: "orc_session_register",
    description: "Register or refresh an agent session and its contract.",
    inputSchema: objectSchema(
      {
        harness: { type: "string" },
        model: { type: "string" },
        role: { type: "string" },
        title: { type: "string" },
        purpose: { type: "string" },
        goal: { type: "string" },
        expectedOutput: { type: "string" },
        successCriteria: { items: { type: "string" }, type: "array" },
        parentId: { type: "string" },
        runId: { type: "string" },
        nodeId: { type: "string" },
      },
      ["harness", "role", "goal"],
    ),
  },
  {
    name: "orc_run_agent_set",
    description: "Set the harness and model used for one agent role in a run.",
    inputSchema: objectSchema(
      {
        runId: { type: "string" },
        role: { type: "string" },
        harness: { type: "string" },
        model: { type: "string" },
      },
      ["runId", "role", "harness"],
    ),
  },
  {
    name: "orc_session_update",
    description: "Update a session lifecycle status.",
    inputSchema: objectSchema(
      { id: { type: "string" }, status: { type: "string" } },
      ["id", "status"],
    ),
  },
  {
    name: "orc_run_create",
    description: "Create a workflow run owned by the orchestrator.",
    inputSchema: objectSchema(
      {
        name: { type: "string" },
        goal: { type: "string" },
        expectedOutput: { type: "string" },
        harness: { type: "string" },
        model: { type: "string" },
      },
      ["name", "goal", "expectedOutput"],
    ),
  },
  {
    name: "orc_run_list",
    description: "List workflow runs in recent order.",
    inputSchema: objectSchema({}),
  },
  {
    name: "orc_run_get",
    description: "Return one workflow run and its graph.",
    inputSchema: objectSchema({ id: { type: "string" } }, ["id"]),
  },
  {
    name: "orc_run_update",
    description: "Update a workflow run lifecycle status.",
    inputSchema: objectSchema(
      { id: { type: "string" }, status: { type: "string" } },
      ["id", "status"],
    ),
  },
  {
    name: "orc_node_upsert",
    description: "Create or replace a workflow node and its dependency edges.",
    inputSchema: objectSchema(
      {
        runId: { type: "string" },
        id: { type: "string" },
        name: { type: "string" },
        purpose: { type: "string" },
        role: { type: "string" },
        harness: { type: "string" },
        model: { type: "string" },
        goal: { type: "string" },
        expectedOutput: { type: "string" },
        successCriteria: { items: { type: "string" }, type: "array" },
        completion: { type: "string" },
        reviewBy: { type: "string" },
        sessionId: { type: "string" },
        status: { type: "string" },
        attempt: { type: "number" },
        dependsOn: { items: { type: "string" }, type: "array" },
      },
      ["runId", "id", "name", "role", "goal", "expectedOutput"],
    ),
  },
  {
    name: "orc_node_update",
    description: "Update a workflow node lifecycle status.",
    inputSchema: objectSchema(
      {
        runId: { type: "string" },
        id: { type: "string" },
        status: { type: "string" },
      },
      ["runId", "id", "status"],
    ),
  },
];

const record = (value: unknown): Readonly<Record<string, unknown>> =>
  typeof value === "object" && value !== null
    ? (value as Readonly<Record<string, unknown>>)
    : {};

const string = (
  value: Readonly<Record<string, unknown>>,
  name: string,
  fallback = "",
): string => (typeof value[name] === "string" ? value[name] : fallback);

const strings = (
  value: Readonly<Record<string, unknown>>,
  name: string,
): ReadonlyArray<string> =>
  Array.isArray(value[name])
    ? value[name].filter((item): item is string => typeof item === "string")
    : [];

const status = (value: Readonly<Record<string, unknown>>): LifecycleStatus =>
  string(value, "status", "working") as LifecycleStatus;

const role = (value: Readonly<Record<string, unknown>>): SessionRole =>
  string(value, "role", "worker") as SessionRole;

const effect = <A>(program: Effect.Effect<A, unknown, never>): Promise<A> =>
  Effect.runPromise(program);

const scoped = <A>(
  program: Effect.Effect<A, unknown, import("./state.ts").StateStoreService>,
): Promise<A> => effect(program.pipe(Effect.provide(StateStoreLive)));

const activeContext = async () => {
  const scope = process.env.ORC_SCOPE;
  if (!scope) return null;
  const state = await scoped(readWorkspace(scope));
  const session = currentSession(state);
  return state.active && session
    ? { scope: state.scope, session, state }
    : null;
};

const toolResult = (value: unknown) => ({
  content: [{ text: JSON.stringify(value, null, 2), type: "text" }],
  structuredContent: value,
});

const callTool = async (name: string, input: unknown): Promise<unknown> => {
  const context = await activeContext();
  if (!context)
    throw new Error(
      "Orc tools require a registered session in an active Orc scope",
    );
  const args = record(input);
  switch (name) {
    case "orc_current_session":
      return toolResult(context.session);
    case "orc_session_list":
      return toolResult(context.state.sessions);
    case "orc_run_list":
      return toolResult(context.state.runs);
    case "orc_run_get": {
      const run = context.state.runs.find(
        (candidate) => candidate.id === string(args, "id"),
      );
      if (!run) throw new Error(`unknown run: ${string(args, "id")}`);
      return toolResult(run);
    }
    case "orc_session_register":
      return toolResult(
        await scoped(
          registerSession({
            completion: "orchestrator",
            expectedOutput: string(args, "expectedOutput", "A verified result"),
            goal: string(args, "goal"),
            harness: string(args, "harness"),
            model: string(args, "model") || undefined,
            hookInput: false,
            id: undefined,
            nativeId: undefined,
            nodeId: string(args, "nodeId") || undefined,
            parentId: string(args, "parentId") || undefined,
            purpose: string(
              args,
              "purpose",
              string(args, "title", "Agent session"),
            ),
            quiet: true,
            reviewBy: undefined,
            role: role(args),
            runId: string(args, "runId") || undefined,
            scope: context.scope,
            source: "managed",
            successCriteria: strings(args, "successCriteria"),
            tag: "session-register",
            title: string(
              args,
              "title",
              string(args, "purpose", "Agent session"),
            ),
            providerRef: undefined,
          }),
        ),
      );
    case "orc_session_update":
      return toolResult(
        await scoped(
          updateSessionStatus({
            id: string(args, "id"),
            scope: context.scope,
            status: status(args),
            tag: "session-update",
          }),
        ),
      );
    case "orc_run_create":
      return toolResult(
        await scoped(
          createRun({
            expectedOutput: string(args, "expectedOutput"),
            goal: string(args, "goal"),
            harness: string(args, "harness") || undefined,
            model: string(args, "model") || undefined,
            name: string(args, "name"),
            orchestratorId: context.session.id,
            scope: context.scope,
            tag: "run-create",
          }),
        ),
      );
    case "orc_run_agent_set":
      return toolResult(
        await scoped(
          setRunAgent({
            harness: string(args, "harness"),
            id: string(args, "runId"),
            model: string(args, "model") || undefined,
            role: role(args),
            scope: context.scope,
            tag: "run-agent-set",
          }),
        ),
      );
    case "orc_run_update":
      return toolResult(
        await scoped(
          updateRunStatus({
            id: string(args, "id"),
            scope: context.scope,
            status: status(args),
            tag: "run-update",
          }),
        ),
      );
    case "orc_node_update":
      return toolResult(
        await scoped(
          updateNodeStatus({
            id: string(args, "id"),
            runId: string(args, "runId"),
            scope: context.scope,
            status: status(args),
            tag: "node-update",
          }),
        ),
      );
    case "orc_node_upsert":
      return toolResult(
        await scoped(
          upsertNode({
            attempt: typeof args.attempt === "number" ? args.attempt : 1,
            completion:
              string(args, "completion") === "judge" ? "judge" : "orchestrator",
            dependsOn: strings(args, "dependsOn"),
            expectedOutput: string(args, "expectedOutput"),
            goal: string(args, "goal"),
            harness: string(args, "harness"),
            id: string(args, "id"),
            model: string(args, "model") || undefined,
            purpose: string(args, "purpose", string(args, "name")),
            reviewBy: string(args, "reviewBy") || undefined,
            role: role(args),
            runId: string(args, "runId"),
            scope: context.scope,
            sessionId: string(args, "sessionId") || undefined,
            status: status(args),
            successCriteria: strings(args, "successCriteria"),
            tag: "node-upsert",
            title: string(args, "name"),
          }),
        ),
      );
    default:
      throw new Error(`unknown tool: ${name}`);
  }
};

const response = (id: JsonRpcRequest["id"], result: unknown): string =>
  JSON.stringify({ id: id ?? null, jsonrpc: "2.0", result });

const errorResponse = (id: JsonRpcRequest["id"], cause: unknown): string =>
  JSON.stringify({
    error: {
      code: -32603,
      message: cause instanceof Error ? cause.message : String(cause),
    },
    id: id ?? null,
    jsonrpc: "2.0",
  });

export const handleMcpRequest = async (
  request: JsonRpcRequest,
): Promise<string | null> => {
  if (request.id === undefined) return null;
  try {
    if (request.method === "initialize")
      return response(request.id, {
        capabilities: { tools: { listChanged: false } },
        protocolVersion: "2025-06-18",
        serverInfo: { name: "orc", version: "0.2.0" },
      });
    if (request.method === "ping") return response(request.id, {});
    if (request.method === "tools/list")
      return response(request.id, {
        tools: (await activeContext()) ? tools : [],
      });
    if (request.method === "tools/call") {
      const params = record(request.params);
      return response(
        request.id,
        await callTool(string(params, "name"), params.arguments),
      );
    }
    return JSON.stringify({
      error: { code: -32601, message: `method not found: ${request.method}` },
      id: request.id,
      jsonrpc: "2.0",
    });
  } catch (cause) {
    return errorResponse(request.id, cause);
  }
};

export const runMcp = async (): Promise<void> => {
  const decoder = new TextDecoder();
  let buffered = "";
  const reader = Bun.stdin.stream().getReader();
  while (true) {
    const next = await reader.read();
    if (next.done) break;
    buffered += decoder.decode(next.value, { stream: true });
    const lines = buffered.split("\n");
    buffered = lines.pop() ?? "";
    for (const line of lines) {
      if (!line.trim()) continue;
      const request = JSON.parse(line) as JsonRpcRequest;
      const output = await handleMcpRequest(request);
      if (output) process.stdout.write(`${output}\n`);
    }
  }
  if (buffered.trim()) {
    const output = await handleMcpRequest(
      JSON.parse(buffered) as JsonRpcRequest,
    );
    if (output) process.stdout.write(`${output}\n`);
  }
};
