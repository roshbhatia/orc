use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    config::Config,
    control, daemon,
    domain::{CompletionTarget, LifecycleStatus, RegistrationSource, RunMode, SessionRole},
    preferences::{self, AutonomyMode},
    state, workflow,
};

struct ToolDefinition {
    name: &'static str,
    description: &'static str,
    properties: fn() -> Value,
    required: &'static [&'static str],
}

impl ToolDefinition {
    fn json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": {
                "type": "object",
                "properties": (self.properties)(),
                "required": self.required,
                "additionalProperties": false,
            },
        })
    }
}

fn no_properties() -> Value {
    json!({})
}

fn id_property() -> Value {
    json!({ "id": { "type": "string" } })
}

fn lifecycle_properties() -> Value {
    json!({
        "id": { "type": "string" },
        "status": { "type": "string" },
    })
}

fn session_registration_properties() -> Value {
    json!({
        "harness": { "type": "string" },
        "model": { "type": "string" },
        "role": { "type": "string" },
        "title": { "type": "string" },
        "purpose": { "type": "string" },
        "goal": { "type": "string" },
        "expectedOutput": { "type": "string" },
        "successCriteria": { "type": "array", "items": { "type": "string" } },
        "nativeId": { "type": "string" },
        "parentId": { "type": "string" },
        "runId": { "type": "string" },
        "nodeId": { "type": "string" },
        "runtimeTimeoutSeconds": { "type": "integer", "minimum": 0 },
        "idleTimeoutSeconds": { "type": "integer", "minimum": 0 },
    })
}

fn run_creation_properties() -> Value {
    json!({
        "name": { "type": "string" },
        "goal": { "type": "string" },
        "expectedOutput": { "type": "string" },
        "harness": { "type": "string" },
        "model": { "type": "string" },
    })
}

fn run_approval_properties() -> Value {
    json!({
        "id": { "type": "string" },
        "gateId": { "type": "string" },
        "resume": { "type": "boolean" },
    })
}

fn workflow_proposal_properties() -> Value {
    json!({ "definition": { "type": "object" } })
}

fn workflow_start_properties() -> Value {
    json!({
        "name": { "type": "string" },
        "background": { "type": "boolean" },
    })
}

fn node_identity_properties() -> Value {
    json!({
        "runId": { "type": "string" },
        "id": { "type": "string" },
        "status": { "type": "string" },
    })
}

fn node_upsert_properties() -> Value {
    json!({
        "runId": { "type": "string" },
        "id": { "type": "string" },
        "name": { "type": "string" },
        "purpose": { "type": "string" },
        "role": { "type": "string" },
        "harness": { "type": "string" },
        "model": { "type": "string" },
        "goal": { "type": "string" },
        "expectedOutput": { "type": "string" },
        "successCriteria": { "type": "array", "items": { "type": "string" } },
        "completion": { "type": "string" },
        "reviewBy": { "type": "string" },
        "sessionId": { "type": "string" },
        "status": { "type": "string" },
        "attempt": { "type": "integer" },
        "dependsOn": { "type": "array", "items": { "type": "string" } },
        "execution": { "type": "string" },
        "judgePolicy": { "type": "string", "enum": ["llm", "human", "llm+human"] },
    })
}

fn node_report_properties() -> Value {
    json!({
        "runId": { "type": "string" },
        "id": { "type": "string" },
        "status": { "type": "string" },
        "output": {},
        "message": { "type": "string" },
        "tokens": { "type": "integer" },
        "costUsd": { "type": "number" },
    })
}

fn tools() -> Value {
    const TOOLS: &[ToolDefinition] = &[
        ToolDefinition {
            name: "orc_current_session",
            description: "Return this harness process' Orc session.",
            properties: no_properties,
            required: &[],
        },
        ToolDefinition {
            name: "orc_session_list",
            description: "List sessions in the active Orc scope.",
            properties: no_properties,
            required: &[],
        },
        ToolDefinition {
            name: "orc_session_register",
            description: "Register an agent session and its contract.",
            properties: session_registration_properties,
            required: &["harness", "role", "goal"],
        },
        ToolDefinition {
            name: "orc_session_update",
            description: "Update a session lifecycle status.",
            properties: lifecycle_properties,
            required: &["id", "status"],
        },
        ToolDefinition {
            name: "orc_session_keepalive",
            description: "Renew a managed session's idle lease after verifying that useful work continues.",
            properties: id_property,
            required: &["id"],
        },
        ToolDefinition {
            name: "orc_session_prune",
            description: "Stop an active agent through an advertised provider, then archive it.",
            properties: id_property,
            required: &["id"],
        },
        ToolDefinition {
            name: "orc_run_create",
            description: "Create a workflow run owned by the orchestrator.",
            properties: run_creation_properties,
            required: &["name", "goal", "expectedOutput"],
        },
        ToolDefinition {
            name: "orc_run_list",
            description: "List workflow runs by recency.",
            properties: no_properties,
            required: &[],
        },
        ToolDefinition {
            name: "orc_run_get",
            description: "Return one workflow run and graph.",
            properties: id_property,
            required: &["id"],
        },
        ToolDefinition {
            name: "orc_run_update",
            description: "Update a workflow run lifecycle status.",
            properties: lifecycle_properties,
            required: &["id", "status"],
        },
        ToolDefinition {
            name: "orc_run_approve",
            description: "Approve one pending human gate and optionally resume the run.",
            properties: run_approval_properties,
            required: &["id"],
        },
        ToolDefinition {
            name: "orc_run_cancel",
            description: "Cancel a workflow run and its local executor.",
            properties: id_property,
            required: &["id"],
        },
        ToolDefinition {
            name: "orc_workflow_propose",
            description: "Validate and version a proposed deterministic workflow without executing it.",
            properties: workflow_proposal_properties,
            required: &["definition"],
        },
        ToolDefinition {
            name: "orc_workflow_start",
            description: "Materialize a versioned workflow proposal and start it when workspace autonomy permits.",
            properties: workflow_start_properties,
            required: &["name"],
        },
        ToolDefinition {
            name: "orc_node_upsert",
            description: "Create or replace a workflow node and dependencies.",
            properties: node_upsert_properties,
            required: &["runId", "id", "name", "role", "goal", "expectedOutput"],
        },
        ToolDefinition {
            name: "orc_node_update",
            description: "Update a workflow node lifecycle status.",
            properties: node_identity_properties,
            required: &["runId", "id", "status"],
        },
        ToolDefinition {
            name: "orc_node_report",
            description: "Report a node result, activity message, token count, and cost.",
            properties: node_report_properties,
            required: &["runId", "id", "status"],
        },
    ];

    Value::Array(TOOLS.iter().map(ToolDefinition::json).collect())
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

fn optional_u64(value: &Value, name: &str) -> Option<u64> {
    value.get(name).and_then(Value::as_u64)
}

fn active_context(
    scope: &std::path::Path,
    session_id: Option<&str>,
) -> Result<(crate::domain::WorkspaceState, crate::domain::Session)> {
    let session_id = session_id.context(
        "ORC_SESSION_ID is required for Orc MCP tools; connect this harness session first",
    )?;
    control::ensure_active_context_for(scope, session_id)
}

fn call(name: &str, input: &Value, config: &Config) -> Result<Value> {
    let scope = std::env::var("ORC_SCOPE").context("ORC_SCOPE is required for Orc MCP tools")?;
    let scope = state::resolve_scope(scope)?;
    let session_id = std::env::var("ORC_SESSION_ID").ok();
    let (workspace, current) = active_context(&scope, session_id.as_deref())?;
    if orchestrator_only(name) && current.role != SessionRole::Orchestrator {
        bail!("only the orchestrator can call {name}");
    }
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
            if role == SessionRole::Orchestrator {
                bail!("managed child sessions cannot have the orchestrator role");
            }
            let session = control::register_managed(
                config,
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
                    native_id: Some(
                        optional(input, "nativeId")
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    ),
                    parent_id: optional(input, "parentId"),
                    run_id: optional(input, "runId"),
                    node_id: optional(input, "nodeId"),
                    runtime_timeout_seconds: optional_u64(input, "runtimeTimeoutSeconds"),
                    idle_timeout_seconds: optional_u64(input, "idleTimeoutSeconds"),
                    source: RegistrationSource::Managed,
                    ..control::SessionLink::default()
                },
            )?;
            daemon::ensure_running(config)?;
            serde_json::to_value(session)?
        }
        "orc_session_update" => {
            let id = string(input, "id");
            let status = string(input, "status")
                .parse::<LifecycleStatus>()
                .map_err(anyhow::Error::msg)?;
            if status == LifecycleStatus::Cancelled {
                serde_json::to_value(control::terminate(
                    config,
                    &scope,
                    &id,
                    "cancelled by orchestrator",
                )?)?
            } else {
                serde_json::to_value(control::update_session(&scope, &id, status)?)?
            }
        }
        "orc_session_keepalive" => {
            let session = control::keepalive(&scope, &string(input, "id"))?;
            daemon::ensure_running(config)?;
            serde_json::to_value(session)?
        }
        "orc_session_prune" => {
            serde_json::to_value(control::prune(config, &scope, &string(input, "id"))?)?
        }
        "orc_run_create" => serde_json::to_value(control::create_run(
            &scope,
            string(input, "name"),
            string(input, "goal"),
            string(input, "expectedOutput"),
            Some(current.id),
            optional(input, "harness"),
            optional(input, "model"),
        )?)?,
        "orc_run_update" => {
            let id = string(input, "id");
            let status = string(input, "status")
                .parse::<LifecycleStatus>()
                .map_err(anyhow::Error::msg)?;
            if status == LifecycleStatus::Cancelled {
                serde_json::to_value(workflow::cancel(config, &scope, &id)?)?
            } else {
                serde_json::to_value(control::update_run(&scope, &id, status)?)?
            }
        }
        "orc_run_approve" => serde_json::to_value(workflow::approve_as(
            config,
            &scope,
            &string(input, "id"),
            optional(input, "gateId").as_deref(),
            input.get("resume").and_then(Value::as_bool).unwrap_or(true),
            workflow::ApprovalActor::Orchestrator,
        )?)?,
        "orc_run_cancel" => {
            serde_json::to_value(workflow::cancel(config, &scope, &string(input, "id"))?)?
        }
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
            let autonomy = preferences::read(&scope)?.autonomy;
            serde_json::to_value(if autonomy != AutonomyMode::Autonomous {
                run
            } else if background {
                workflow::spawn(config, &scope, &run.id)?
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
                execution: optional(input, "execution"),
                judge_policy: optional(input, "judgePolicy")
                    .unwrap_or_else(|| "llm".into())
                    .parse::<crate::domain::JudgePolicy>()
                    .map_err(anyhow::Error::msg)?,
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
        "orc_node_report" => {
            let run_id = string(input, "runId");
            let node_id = string(input, "id");
            serde_json::to_value(control::report_node(
                &scope,
                &run_id,
                &node_id,
                (current.role != SessionRole::Orchestrator).then_some(current.id.as_str()),
                control::NodeReport {
                    status: string(input, "status")
                        .parse::<LifecycleStatus>()
                        .map_err(anyhow::Error::msg)?,
                    output: input.get("output").cloned(),
                    message: optional(input, "message"),
                    tokens: input.get("tokens").and_then(Value::as_u64),
                    cost_usd: input.get("costUsd").and_then(Value::as_f64),
                },
            )?)?
        }
        _ => bail!("unknown tool: {name}"),
    };
    Ok(text_result(value))
}

fn orchestrator_only(name: &str) -> bool {
    matches!(
        name,
        "orc_session_register"
            | "orc_session_update"
            | "orc_session_keepalive"
            | "orc_session_prune"
            | "orc_run_create"
            | "orc_run_update"
            | "orc_run_approve"
            | "orc_run_cancel"
            | "orc_workflow_propose"
            | "orc_workflow_start"
            | "orc_node_upsert"
            | "orc_node_update"
    )
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn tool_catalog_has_unique_named_definitions() {
        let catalog = tools().as_array().expect("tool catalog").clone();
        let names = catalog
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();

        assert_eq!(catalog.len(), names.len());
        assert_eq!(names.len(), 17);
        assert!(catalog.iter().all(|tool| {
            tool.pointer("/inputSchema/additionalProperties") == Some(&Value::Bool(false))
        }));
    }

    #[test]
    fn mcp_context_requires_an_explicit_session_identity() {
        let error = active_context(std::path::Path::new("."), None)
            .expect_err("MCP must not inherit the latest orchestrator");

        assert!(error.to_string().contains("ORC_SESSION_ID is required"));
    }
}
