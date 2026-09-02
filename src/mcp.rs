use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    config::Config,
    control,
    domain::{CompletionTarget, LifecycleStatus, RegistrationSource, RunMode, SessionRole},
    state, workflow,
};

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

fn tools() -> Value {
    json!([
        { "name": "orc_current_session", "description": "Return this harness process' Orc session.", "inputSchema": schema(json!({}), &[]) },
        { "name": "orc_session_list", "description": "List sessions in the active Orc scope.", "inputSchema": schema(json!({}), &[]) },
        { "name": "orc_session_register", "description": "Register an agent session and its contract.", "inputSchema": schema(json!({
            "harness": {"type":"string"}, "model": {"type":"string"}, "role": {"type":"string"}, "title": {"type":"string"},
            "purpose": {"type":"string"}, "goal": {"type":"string"}, "expectedOutput": {"type":"string"},
            "successCriteria": {"type":"array", "items":{"type":"string"}}, "parentId": {"type":"string"}, "runId": {"type":"string"}, "nodeId": {"type":"string"}
        }), &["harness", "role", "goal"]) },
        { "name": "orc_session_update", "description": "Update a session lifecycle status.", "inputSchema": schema(json!({"id":{"type":"string"},"status":{"type":"string"}}), &["id","status"]) },
        { "name": "orc_run_create", "description": "Create a workflow run owned by the orchestrator.", "inputSchema": schema(json!({"name":{"type":"string"},"goal":{"type":"string"},"expectedOutput":{"type":"string"},"harness":{"type":"string"},"model":{"type":"string"}}), &["name","goal","expectedOutput"]) },
        { "name": "orc_run_list", "description": "List workflow runs by recency.", "inputSchema": schema(json!({}), &[]) },
        { "name": "orc_run_get", "description": "Return one workflow run and graph.", "inputSchema": schema(json!({"id":{"type":"string"}}), &["id"]) },
        { "name": "orc_run_update", "description": "Update a workflow run lifecycle status.", "inputSchema": schema(json!({"id":{"type":"string"},"status":{"type":"string"}}), &["id","status"]) },
        { "name": "orc_run_approve", "description": "Approve one pending human gate and optionally resume the run.", "inputSchema": schema(json!({"id":{"type":"string"},"gateId":{"type":"string"},"resume":{"type":"boolean"}}), &["id"]) },
        { "name": "orc_run_cancel", "description": "Cancel a workflow run and its local executor.", "inputSchema": schema(json!({"id":{"type":"string"}}), &["id"]) },
        { "name": "orc_workflow_propose", "description": "Validate and version a proposed deterministic workflow without executing it.", "inputSchema": schema(json!({"definition":{"type":"object"}}), &["definition"]) },
        { "name": "orc_workflow_start", "description": "Materialize and start a versioned workflow for this directory.", "inputSchema": schema(json!({"name":{"type":"string"},"background":{"type":"boolean"}}), &["name"]) },
        { "name": "orc_node_upsert", "description": "Create or replace a workflow node and dependencies.", "inputSchema": schema(json!({
            "runId":{"type":"string"},"id":{"type":"string"},"name":{"type":"string"},"purpose":{"type":"string"},"role":{"type":"string"},
            "harness":{"type":"string"},"model":{"type":"string"},"goal":{"type":"string"},"expectedOutput":{"type":"string"},
            "successCriteria":{"type":"array","items":{"type":"string"}},"completion":{"type":"string"},"reviewBy":{"type":"string"},
            "sessionId":{"type":"string"},"status":{"type":"string"},"attempt":{"type":"integer"},"dependsOn":{"type":"array","items":{"type":"string"}}
        }), &["runId","id","name","role","goal","expectedOutput"]) },
        { "name": "orc_node_update", "description": "Update a workflow node lifecycle status.", "inputSchema": schema(json!({"runId":{"type":"string"},"id":{"type":"string"},"status":{"type":"string"}}), &["runId","id","status"]) }
        ,{ "name": "orc_node_report", "description": "Report a node result, activity message, token count, and cost.", "inputSchema": schema(json!({"runId":{"type":"string"},"id":{"type":"string"},"status":{"type":"string"},"output":{},"message":{"type":"string"},"tokens":{"type":"integer"},"costUsd":{"type":"number"}}), &["runId","id","status"]) }
    ])
}

fn text_result(value: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }], "structuredContent": value })
}

fn string(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
fn optional(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
fn strings(value: &Value, name: &str) -> Vec<String> {
    value
        .get(name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn call(name: &str, input: &Value, config: &Config) -> Result<Value> {
    let scope = std::env::var("ORC_SCOPE").context("ORC_SCOPE is required for Orc MCP tools")?;
    let scope = state::resolve_scope(scope)?;
    let (workspace, current) = control::ensure_active_context(&scope)?;
    let value = match name {
        "orc_current_session" => serde_json::to_value(current)?,
        "orc_session_list" => serde_json::to_value(workspace.sessions)?,
        "orc_run_list" => serde_json::to_value(workspace.runs)?,
        "orc_run_get" => serde_json::to_value(
            workspace
                .runs
                .iter()
                .find(|run| run.id == string(input, "id"))
                .context("unknown run")?,
        )?,
        "orc_session_register" => {
            let role = string(input, "role")
                .parse::<SessionRole>()
                .map_err(anyhow::Error::msg)?;
            serde_json::to_value(control::register(
                &scope,
                control::Contract {
                    harness: string(input, "harness"),
                    model: optional(input, "model"),
                    role,
                    title: optional(input, "title").unwrap_or_else(|| string(input, "purpose")),
                    purpose: optional(input, "purpose").unwrap_or_else(|| "Agent session".into()),
                    goal: string(input, "goal"),
                    expected_output: optional(input, "expectedOutput")
                        .unwrap_or_else(|| "A verified result".into()),
                    success_criteria: strings(input, "successCriteria"),
                    completion: CompletionTarget::Orchestrator,
                    review_by: optional(input, "reviewBy"),
                },
                control::SessionLink {
                    parent_id: optional(input, "parentId"),
                    run_id: optional(input, "runId"),
                    node_id: optional(input, "nodeId"),
                    source: RegistrationSource::Managed,
                    ..control::SessionLink::default()
                },
            )?)?
        }
        "orc_session_update" => serde_json::to_value(control::update_session(
            &scope,
            &string(input, "id"),
            string(input, "status")
                .parse::<LifecycleStatus>()
                .map_err(anyhow::Error::msg)?,
        )?)?,
        "orc_run_create" => serde_json::to_value(control::create_run(
            &scope,
            string(input, "name"),
            string(input, "goal"),
            string(input, "expectedOutput"),
            Some(current.id),
            optional(input, "harness"),
            optional(input, "model"),
        )?)?,
        "orc_run_update" => serde_json::to_value(control::update_run(
            &scope,
            &string(input, "id"),
            string(input, "status")
                .parse::<LifecycleStatus>()
                .map_err(anyhow::Error::msg)?,
        )?)?,
        "orc_run_approve" => serde_json::to_value(workflow::approve(
            config,
            &scope,
            &string(input, "id"),
            optional(input, "gateId").as_deref(),
            input.get("resume").and_then(Value::as_bool).unwrap_or(true),
        )?)?,
        "orc_run_cancel" => serde_json::to_value(workflow::cancel(&scope, &string(input, "id"))?)?,
        "orc_workflow_propose" => {
            let definition: workflow::Definition = serde_json::from_value(
                input
                    .get("definition")
                    .cloned()
                    .context("definition is required")?,
            )?;
            let path = workflow::save(config, &scope, &definition)?;
            json!({
                "path": path,
                "plan": workflow::plan(config, &scope, &definition)?,
            })
        }
        "orc_workflow_start" => {
            let path = workflow::path(config, &scope, &string(input, "name"))?;
            let background = input
                .get("background")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let run = workflow::materialize(
                config,
                &scope,
                &path,
                if background {
                    RunMode::Background
                } else {
                    RunMode::Foreground
                },
            )?;
            serde_json::to_value(if background {
                workflow::spawn(&scope, &run.id)?
            } else {
                workflow::execute(config, &scope, &run.id)?
            })?
        }
        "orc_node_upsert" => serde_json::to_value(control::upsert_node(
            &scope,
            &string(input, "runId"),
            control::NodeSpec {
                id: string(input, "id"),
                contract: control::Contract {
                    harness: optional(input, "harness").unwrap_or(current.harness),
                    model: optional(input, "model"),
                    role: string(input, "role")
                        .parse::<SessionRole>()
                        .map_err(anyhow::Error::msg)?,
                    title: string(input, "name"),
                    purpose: optional(input, "purpose").unwrap_or_else(|| "Workflow step".into()),
                    goal: string(input, "goal"),
                    expected_output: string(input, "expectedOutput"),
                    success_criteria: strings(input, "successCriteria"),
                    completion: optional(input, "completion")
                        .unwrap_or_else(|| "orchestrator".into())
                        .parse::<CompletionTarget>()
                        .map_err(anyhow::Error::msg)?,
                    review_by: optional(input, "reviewBy"),
                },
                session_id: optional(input, "sessionId"),
                status: optional(input, "status")
                    .unwrap_or_else(|| "queued".into())
                    .parse::<LifecycleStatus>()
                    .map_err(anyhow::Error::msg)?,
                attempt: input.get("attempt").and_then(Value::as_u64).unwrap_or(0) as u32,
                depends_on: strings(input, "dependsOn"),
            },
        )?)?,
        "orc_node_update" => serde_json::to_value(control::update_node(
            &scope,
            &string(input, "runId"),
            &string(input, "id"),
            string(input, "status")
                .parse::<LifecycleStatus>()
                .map_err(anyhow::Error::msg)?,
        )?)?,
        "orc_node_report" => serde_json::to_value(control::report_node(
            &scope,
            &string(input, "runId"),
            &string(input, "id"),
            control::NodeReport {
                status: string(input, "status")
                    .parse::<LifecycleStatus>()
                    .map_err(anyhow::Error::msg)?,
                output: input.get("output").cloned(),
                message: optional(input, "message"),
                tokens: input.get("tokens").and_then(Value::as_u64),
                cost_usd: input.get("costUsd").and_then(Value::as_f64),
            },
        )?)?,
        _ => bail!("unknown tool: {name}"),
    };
    Ok(text_result(value))
}

pub fn run(config: Config) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line)?;
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = match method {
            "initialize" => {
                json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"orc","version":crate::VERSION}}})
            }
            "notifications/initialized" => continue,
            "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools()}}),
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
                match call(
                    params
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    params.get("arguments").unwrap_or(&Value::Null),
                    &config,
                ) {
                    Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                    Err(error) => {
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":format!("{error:#}")}})
                    }
                }
            }
            _ => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}})
            }
        };
        serde_json::to_writer(&mut stdout, &response)?;
        writeln!(stdout)?;
        stdout.flush()?;
    }
    Ok(())
}
