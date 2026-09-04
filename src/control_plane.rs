use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    config::{self, Config},
    provider::{self, Capability, Manifest},
    state,
};

pub const API_VERSION: &str = "orc.dev/v1alpha1";
const STORE_VERSION: &str = "orc.control/v1";

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
pub enum ResourceKind {
    Workflow,
    Run,
    Session,
    Execution,
    EventBinding,
    Artifact,
}

impl ResourceKind {
    pub fn plural(self) -> &'static str {
        match self {
            Self::Workflow => "workflows",
            Self::Run => "runs",
            Self::Session => "sessions",
            Self::Execution => "executions",
            Self::EventBinding => "eventbindings",
            Self::Artifact => "artifacts",
        }
    }
}

impl std::fmt::Display for ResourceKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl FromStr for ResourceKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().trim_end_matches('s') {
            "workflow" => Ok(Self::Workflow),
            "run" => Ok(Self::Run),
            "session" => Ok(Self::Session),
            "execution" => Ok(Self::Execution),
            "eventbinding" | "event-binding" => Ok(Self::EventBinding),
            "artifact" => Ok(Self::Artifact),
            _ => bail!("unknown resource kind: {value}"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectMeta {
    pub name: String,
    #[serde(default)]
    pub uid: String,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub resource_version: u64,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub field_owners: BTreeMap<String, String>,
    #[serde(default = "epoch")]
    pub created_at: DateTime<Utc>,
    #[serde(default = "epoch")]
    pub updated_at: DateTime<Utc>,
}

fn epoch() -> DateTime<Utc> {
    DateTime::UNIX_EPOCH
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ResourceStatus {
    pub phase: String,
    pub observed_generation: u64,
    pub provider: Option<String>,
    pub external_ref: Option<String>,
    pub message: Option<String>,
    pub outputs: BTreeMap<String, ArtifactReference>,
    pub delivered_events: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactReference {
    pub digest: String,
    pub size: u64,
    pub media_type: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub api_version: String,
    pub kind: ResourceKind,
    pub metadata: ObjectMeta,
    #[schemars(with = "BTreeMap<String, Value>")]
    pub spec: Value,
    #[serde(default)]
    pub status: ResourceStatus,
}

impl Resource {
    fn key(&self) -> ResourceKey {
        ResourceKey {
            kind: self.kind,
            name: self.metadata.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceKey {
    pub kind: ResourceKind,
    pub name: String,
}

impl std::fmt::Display for ResourceKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.kind.plural(), self.name)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEvent {
    pub sequence: u64,
    pub id: String,
    pub event_type: String,
    pub reason: String,
    pub subject: ResourceKey,
    pub message: String,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredArtifact {
    reference: ArtifactReference,
    body: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlStore {
    version: String,
    scope: String,
    resource_version: u64,
    event_sequence: u64,
    #[serde(default)]
    resources: BTreeMap<String, Resource>,
    #[serde(default)]
    events: Vec<ControlEvent>,
    #[serde(default)]
    artifacts: BTreeMap<String, StoredArtifact>,
}

impl ControlStore {
    fn empty(scope: &Path) -> Self {
        Self {
            version: STORE_VERSION.into(),
            scope: scope.display().to_string(),
            resource_version: 0,
            event_sequence: 0,
            resources: BTreeMap::new(),
            events: Vec::new(),
            artifacts: BTreeMap::new(),
        }
    }

    fn emit(&mut self, event_type: &str, reason: &str, subject: ResourceKey, message: String) {
        self.event_sequence += 1;
        self.events.push(ControlEvent {
            sequence: self.event_sequence,
            id: Uuid::new_v4().to_string(),
            event_type: event_type.into(),
            reason: reason.into(),
            subject,
            message,
            at: Utc::now(),
        });
    }

    fn resource(&self, key: &ResourceKey) -> Option<&Resource> {
        self.resources.get(&key.to_string())
    }

    fn resource_mut(&mut self, key: &ResourceKey) -> Option<&mut Resource> {
        self.resources.get_mut(&key.to_string())
    }

    fn store_artifact(&mut self, body: String, media_type: String) -> ArtifactReference {
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(body.as_bytes())));
        let reference = ArtifactReference {
            digest: digest.clone(),
            size: body.len() as u64,
            media_type,
        };
        self.artifacts
            .entry(digest)
            .or_insert_with(|| StoredArtifact {
                reference: reference.clone(),
                body,
            });
        reference
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeAction {
    Create,
    Configure,
    Unchanged,
    Delete,
}

impl std::fmt::Display for ChangeAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", format!("{self:?}").to_ascii_lowercase())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceChange {
    pub resource: ResourceKey,
    pub action: ChangeAction,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub dry_run: bool,
    pub changes: Vec<ResourceChange>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileResult {
    pub dry_run: bool,
    pub passes: usize,
    pub actions: Vec<ProviderAction>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAction {
    pub operation_id: String,
    pub capability: Capability,
    pub resource: ResourceKey,
    pub provider: Option<String>,
    pub changed: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
struct ActionOutput {
    provider: String,
    value: Value,
}

fn control_path(scope: &Path) -> PathBuf {
    config::state_home()
        .join("orc/control")
        .join(format!("{}.json", state::scope_key(scope)))
}

fn read_store(scope: &Path) -> Result<ControlStore> {
    let path = control_path(scope);
    match fs::read_to_string(&path) {
        Ok(source) => {
            let store: ControlStore = serde_json::from_str(&source)
                .with_context(|| format!("parse control-plane state {}", path.display()))?;
            if store.version != STORE_VERSION {
                bail!("unsupported control-plane state version: {}", store.version);
            }
            Ok(store)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ControlStore::empty(scope))
        }
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn write_store(scope: &Path, store: &ControlStore) -> Result<()> {
    let target = control_path(scope);
    let parent = target
        .parent()
        .context("control-plane state has no parent")?;
    fs::create_dir_all(parent).context("create control-plane state directory")?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    fs::write(&temporary, serde_json::to_vec_pretty(store)?)
        .context("write temporary control-plane state")?;
    fs::rename(&temporary, &target).context("commit control-plane state")?;
    Ok(())
}

fn update_store<T>(
    scope: &Path,
    operation: impl FnOnce(&mut ControlStore) -> Result<T>,
) -> Result<T> {
    let target = control_path(scope);
    state::with_path_lock(&target, || {
        let mut store = read_store(scope)?;
        let result = operation(&mut store)?;
        write_store(scope, &store)?;
        Ok(result)
    })
}

pub fn schema() -> Value {
    let mut schema =
        serde_json::to_value(schema_for!(Resource)).expect("resource schema serializes");
    schema["properties"]["apiVersion"] = json!({"const": API_VERSION, "type": "string"});
    schema
}

pub fn load_documents(path: &Path) -> Result<Vec<Resource>> {
    let source = if path == Path::new("-") {
        let mut source = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut source)?;
        source
    } else {
        fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    };
    let mut resources = Vec::new();
    for document in serde_yaml::Deserializer::from_str(&source) {
        let resource = Resource::deserialize(document).context("parse Orc resource")?;
        validate_resource(&resource)?;
        resources.push(resource);
    }
    if resources.is_empty() {
        bail!("resource file is empty");
    }
    Ok(resources)
}

fn validate_resource(resource: &Resource) -> Result<()> {
    if resource.api_version != API_VERSION {
        bail!(
            "{} uses unsupported apiVersion {}",
            resource.key(),
            resource.api_version
        );
    }
    if resource.metadata.name.is_empty()
        || !resource
            .metadata
            .name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '.'))
    {
        bail!("{} has an invalid metadata.name", resource.key());
    }
    if !resource.spec.is_object() {
        bail!("{} spec must be an object", resource.key());
    }
    Ok(())
}

fn flatten(value: &Value, prefix: &str, output: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(object) if !object.is_empty() => {
            for (name, value) in object {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                flatten(value, &path, output);
            }
        }
        _ => {
            output.insert(prefix.into(), value.clone());
        }
    }
}

fn changed_paths(old: Option<&Resource>, incoming: &Resource) -> Vec<String> {
    let mut before = BTreeMap::new();
    let mut after = BTreeMap::new();
    if let Some(old) = old {
        flatten(&old.spec, "spec", &mut before);
        flatten(&json!(old.metadata.labels), "metadata.labels", &mut before);
    }
    flatten(&incoming.spec, "spec", &mut after);
    flatten(
        &json!(incoming.metadata.labels),
        "metadata.labels",
        &mut after,
    );
    before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect()
}

pub fn diff(scope: &Path, resources: &[Resource]) -> Result<Vec<ResourceChange>> {
    let scope = state::resolve_scope(scope)?;
    let store = read_store(&scope)?;
    let mut changes = Vec::new();
    for resource in resources {
        let mut resource = resource.clone();
        normalize_artifact(&mut resource)?;
        let current = store.resource(&resource.key());
        let paths = changed_paths(current, &resource);
        changes.push(ResourceChange {
            resource: resource.key(),
            action: if current.is_none() {
                ChangeAction::Create
            } else if paths.is_empty() {
                ChangeAction::Unchanged
            } else {
                ChangeAction::Configure
            },
            paths,
        });
    }
    Ok(changes)
}

pub fn apply(
    scope: &Path,
    resources: Vec<Resource>,
    field_manager: &str,
    force_conflicts: bool,
    dry_run: bool,
) -> Result<ApplyResult> {
    if field_manager.trim().is_empty() {
        bail!("field manager must not be empty");
    }
    let scope = state::resolve_scope(scope)?;
    let operation = |store: &mut ControlStore| {
        let mut changes = Vec::new();
        for mut incoming in resources {
            let artifact = normalize_artifact(&mut incoming)?;
            let key = incoming.key();
            let current = store.resource(&key).cloned();
            let paths = changed_paths(current.as_ref(), &incoming);
            for path in &paths {
                if let Some(owner) = current
                    .as_ref()
                    .and_then(|resource| resource.metadata.field_owners.get(path))
                    .filter(|owner| owner.as_str() != field_manager)
                    && !force_conflicts
                {
                    bail!("field conflict on {key} {path}: owned by {owner}");
                }
            }
            let action = if current.is_none() {
                ChangeAction::Create
            } else if paths.is_empty() {
                ChangeAction::Unchanged
            } else {
                ChangeAction::Configure
            };
            if action != ChangeAction::Unchanged {
                let now = Utc::now();
                store.resource_version += 1;
                incoming.metadata.uid = current
                    .as_ref()
                    .map(|resource| resource.metadata.uid.clone())
                    .filter(|uid| !uid.is_empty())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                incoming.metadata.generation = current
                    .as_ref()
                    .map_or(1, |resource| resource.metadata.generation + 1);
                incoming.metadata.resource_version = store.resource_version;
                incoming.metadata.created_at = current
                    .as_ref()
                    .map_or(now, |resource| resource.metadata.created_at);
                incoming.metadata.updated_at = now;
                if let Some(current) = current {
                    incoming.metadata.field_owners = current.metadata.field_owners;
                    incoming.status = current.status;
                } else {
                    incoming.status = ResourceStatus::default();
                }
                for path in &paths {
                    incoming
                        .metadata
                        .field_owners
                        .insert(path.clone(), field_manager.into());
                }
                if let Some((body, reference)) = artifact {
                    store
                        .artifacts
                        .entry(reference.digest.clone())
                        .or_insert(StoredArtifact { reference, body });
                }
                store.resources.insert(key.to_string(), incoming);
                store.emit(
                    "Normal",
                    if action == ChangeAction::Create {
                        "Created"
                    } else {
                        "Configured"
                    },
                    key.clone(),
                    format!("{key} {action} by {field_manager}"),
                );
            }
            changes.push(ResourceChange {
                resource: key,
                action,
                paths,
            });
        }
        Ok(ApplyResult { dry_run, changes })
    };
    if dry_run {
        let mut store = read_store(&scope)?;
        operation(&mut store)
    } else {
        update_store(&scope, operation)
    }
}

fn normalize_artifact(resource: &mut Resource) -> Result<Option<(String, ArtifactReference)>> {
    if resource.kind != ResourceKind::Artifact {
        return Ok(None);
    }
    let Some(spec) = resource.spec.as_object_mut() else {
        return Ok(None);
    };
    let Some(content) = spec.remove("content") else {
        return Ok(None);
    };
    let body = match content {
        Value::String(body) => body,
        value => serde_json::to_string(&value)?,
    };
    let media_type = spec
        .get("mediaType")
        .and_then(Value::as_str)
        .unwrap_or("text/plain")
        .to_owned();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(body.as_bytes())));
    let reference = ArtifactReference {
        digest,
        size: body.len() as u64,
        media_type,
    };
    spec.insert("digest".into(), json!(reference.digest));
    spec.insert("size".into(), json!(reference.size));
    Ok(Some((body, reference)))
}

pub fn list(scope: &Path, kind: Option<ResourceKind>, name: Option<&str>) -> Result<Vec<Resource>> {
    let scope = state::resolve_scope(scope)?;
    let store = read_store(&scope)?;
    let resources = store
        .resources
        .values()
        .filter(|resource| kind.is_none_or(|kind| resource.kind == kind))
        .filter(|resource| name.is_none_or(|name| resource.metadata.name == name))
        .cloned()
        .collect::<Vec<_>>();
    if name.is_some() && resources.is_empty() {
        bail!("resource not found");
    }
    Ok(resources)
}

pub fn artifact_body(scope: &Path, name: &str) -> Result<String> {
    let scope = state::resolve_scope(scope)?;
    let store = read_store(&scope)?;
    let resource = store
        .resource(&ResourceKey {
            kind: ResourceKind::Artifact,
            name: name.into(),
        })
        .with_context(|| format!("artifact/{name} not found"))?;
    let digest = resource
        .spec
        .get("digest")
        .and_then(Value::as_str)
        .context("artifact has no digest")?;
    store
        .artifacts
        .get(digest)
        .map(|artifact| artifact.body.clone())
        .context("artifact body is missing")
}

pub fn delete(
    scope: &Path,
    kind: ResourceKind,
    name: &str,
    dry_run: bool,
) -> Result<ResourceChange> {
    let scope = state::resolve_scope(scope)?;
    let key = ResourceKey {
        kind,
        name: name.into(),
    };
    let operation = |store: &mut ControlStore| {
        if !store.resources.contains_key(&key.to_string()) {
            bail!("{key} not found");
        }
        store.resources.remove(&key.to_string());
        store.resource_version += 1;
        store.emit("Normal", "Deleted", key.clone(), format!("{key} deleted"));
        Ok(ResourceChange {
            resource: key.clone(),
            action: ChangeAction::Delete,
            paths: Vec::new(),
        })
    };
    if dry_run {
        let mut store = read_store(&scope)?;
        operation(&mut store)
    } else {
        update_store(&scope, operation)
    }
}

pub fn events(
    scope: &Path,
    kind: Option<ResourceKind>,
    name: Option<&str>,
    after: u64,
) -> Result<Vec<ControlEvent>> {
    let scope = state::resolve_scope(scope)?;
    Ok(read_store(&scope)?
        .events
        .into_iter()
        .filter(|event| event.sequence > after)
        .filter(|event| kind.is_none_or(|kind| event.subject.kind == kind))
        .filter(|event| name.is_none_or(|name| event.subject.name == name))
        .collect())
}

pub fn watch_events(
    scope: &Path,
    kind: Option<ResourceKind>,
    name: Option<&str>,
    after: u64,
    count: usize,
    timeout: Duration,
    mut receive: impl FnMut(ControlEvent) -> Result<()>,
) -> Result<usize> {
    let deadline = Instant::now() + timeout.min(Duration::from_secs(3600));
    let mut cursor = after;
    let mut delivered = 0;
    while delivered < count && Instant::now() < deadline {
        for event in events(scope, kind, name, cursor)? {
            cursor = event.sequence;
            receive(event)?;
            delivered += 1;
            if delivered == count {
                break;
            }
        }
        if delivered < count {
            thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(delivered)
}

fn operation_id(resource: &Resource, capability: Capability) -> String {
    let input = format!(
        "{}:{}:{}:{}",
        resource.metadata.uid, resource.metadata.generation, capability, resource.metadata.name
    );
    format!(
        "orc-{}",
        &hex::encode(Sha256::digest(input.as_bytes()))[..24]
    )
}

fn execution_levels(store: &ControlStore) -> Result<Vec<Vec<ResourceKey>>> {
    let executions = store
        .resources
        .values()
        .filter(|resource| resource.kind == ResourceKind::Execution)
        .collect::<Vec<_>>();
    let known = executions
        .iter()
        .map(|resource| resource.metadata.name.as_str())
        .collect::<BTreeSet<_>>();
    let dependencies = executions
        .iter()
        .map(|resource| {
            let items = resource
                .spec
                .get("dependsOn")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
            for dependency in &items {
                if !known.contains(dependency.as_str()) {
                    bail!(
                        "execution/{} depends on missing execution/{dependency}",
                        resource.metadata.name
                    );
                }
            }
            Ok((resource.metadata.name.clone(), items))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut remaining = dependencies;
    let mut resolved = BTreeSet::new();
    let mut levels = Vec::new();
    while !remaining.is_empty() {
        let level = remaining
            .iter()
            .filter(|(_, dependencies)| dependencies.is_subset(&resolved))
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        if level.is_empty() {
            bail!("execution dependency graph contains a cycle");
        }
        for name in &level {
            remaining.remove(name);
            resolved.insert(name.clone());
        }
        levels.push(
            level
                .into_iter()
                .map(|name| ResourceKey {
                    kind: ResourceKind::Execution,
                    name,
                })
                .collect(),
        );
    }
    Ok(levels)
}

fn resolve_inputs(store: &ControlStore, resource: &Resource) -> Result<Value> {
    let mut inputs = resource
        .spec
        .get("inputs")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let Some(object) = inputs.as_object_mut() else {
        bail!("{} spec.inputs must be an object", resource.key());
    };
    for value in object.values_mut() {
        let digest = if let Some(name) = value.get("artifactRef").and_then(Value::as_str) {
            let artifact = store
                .resource(&ResourceKey {
                    kind: ResourceKind::Artifact,
                    name: name.into(),
                })
                .with_context(|| format!("artifact/{name} not found"))?;
            artifact
                .spec
                .get("digest")
                .and_then(Value::as_str)
                .context("artifact has no digest")?
        } else if let Some(execution) = value.get("executionRef").and_then(Value::as_str) {
            let output = value
                .get("output")
                .and_then(Value::as_str)
                .context("executionRef input requires output")?;
            &store
                .resource(&ResourceKey {
                    kind: ResourceKind::Execution,
                    name: execution.into(),
                })
                .with_context(|| format!("execution/{execution} not found"))?
                .status
                .outputs
                .get(output)
                .with_context(|| format!("execution/{execution} has no output {output}"))?
                .digest
        } else {
            continue;
        };
        let body = &store
            .artifacts
            .get(digest)
            .context("artifact body is missing")?
            .body;
        *value = Value::String(body.clone());
    }
    Ok(inputs)
}

fn provider_request(
    store: &ControlStore,
    scope: &Path,
    resource: &Resource,
    capability: Capability,
    event: Option<&ControlEvent>,
) -> Result<Value> {
    let selected = resource.spec.get("provider").and_then(Value::as_str);
    let providers = selected.map_or_else(
        || json!({}),
        |provider| json!({capability.to_string(): provider}),
    );
    Ok(json!({
        "version": "orc.provider/v1",
        "action": capability.to_string(),
        "capability": capability,
        "operationId": operation_id(resource, capability),
        "scope": scope,
        "resource": resource,
        "inputs": resolve_inputs(store, resource)?,
        "event": event,
        "providers": providers,
        "plan": null,
    }))
}

fn generated_resource(kind: ResourceKind, name: String, spec: Value) -> Resource {
    Resource {
        api_version: API_VERSION.into(),
        kind,
        metadata: ObjectMeta {
            name,
            uid: Uuid::new_v4().to_string(),
            generation: 1,
            resource_version: 0,
            labels: BTreeMap::new(),
            field_owners: BTreeMap::from([("spec".into(), "orc-reconciler".into())]),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        spec,
        status: ResourceStatus {
            phase: "Pending".into(),
            ..ResourceStatus::default()
        },
    }
}

fn reconcile_runs(store: &mut ControlStore) -> Result<bool> {
    let runs = store
        .resources
        .values()
        .filter(|resource| resource.kind == ResourceKind::Run)
        .cloned()
        .collect::<Vec<_>>();
    let mut changed = false;
    for run in runs {
        let workflow_name = run
            .spec
            .get("workflowRef")
            .and_then(Value::as_str)
            .with_context(|| format!("{} spec.workflowRef is required", run.key()))?;
        let workflow = store
            .resource(&ResourceKey {
                kind: ResourceKind::Workflow,
                name: workflow_name.into(),
            })
            .with_context(|| format!("workflow/{workflow_name} not found"))?
            .clone();
        let stages = workflow
            .spec
            .get("stages")
            .and_then(Value::as_array)
            .with_context(|| format!("{} spec.stages is required", workflow.key()))?
            .clone();
        let stage_names = stages
            .iter()
            .map(|stage| {
                stage
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .context("workflow stage name is required")
            })
            .collect::<Result<BTreeSet<_>>>()?;
        for stage in stages {
            let stage_name = stage["name"]
                .as_str()
                .context("workflow stage name is required")?;
            let execution_name = format!("{}-{stage_name}", run.metadata.name);
            let key = ResourceKey {
                kind: ResourceKind::Execution,
                name: execution_name.clone(),
            };
            if store.resource(&key).is_some() {
                continue;
            }
            let mut spec = stage
                .as_object()
                .context("workflow stage must be an object")?
                .clone();
            spec.remove("name");
            let dependencies = spec
                .remove("dependsOn")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .map(|dependency| {
                    let dependency = dependency
                        .as_str()
                        .context("stage dependency must be a string")?;
                    if !stage_names.contains(dependency) {
                        bail!("workflow stage {stage_name} depends on missing stage {dependency}");
                    }
                    Ok(Value::String(format!("{}-{dependency}", run.metadata.name)))
                })
                .collect::<Result<Vec<_>>>()?;
            spec.insert("dependsOn".into(), Value::Array(dependencies));
            spec.insert("runRef".into(), Value::String(run.metadata.name.clone()));
            spec.entry("desiredState")
                .or_insert_with(|| Value::String("running".into()));
            store.resource_version += 1;
            let mut execution =
                generated_resource(ResourceKind::Execution, execution_name, Value::Object(spec));
            execution.metadata.resource_version = store.resource_version;
            store.resources.insert(key.to_string(), execution);
            store.emit(
                "Normal",
                "Created",
                key.clone(),
                format!("{key} materialized from {}", workflow.key()),
            );
            changed = true;
        }
        let run_key = run.key();
        let phase = run_phase(store, &run.metadata.name);
        let current = store.resource_mut(&run_key).context("run disappeared")?;
        if current.status.phase != phase
            || current.status.observed_generation != current.metadata.generation
        {
            current.status.phase = phase.clone();
            current.status.observed_generation = current.metadata.generation;
            store.emit(
                "Normal",
                "RunProgressed",
                run_key.clone(),
                format!("{run_key} is {phase}"),
            );
            changed = true;
        }
    }
    Ok(changed)
}

fn run_phase(store: &ControlStore, run_name: &str) -> String {
    let phases = store
        .resources
        .values()
        .filter(|resource| {
            resource.kind == ResourceKind::Execution
                && resource.spec.get("runRef").and_then(Value::as_str) == Some(run_name)
        })
        .map(|execution| execution.status.phase.as_str())
        .collect::<Vec<_>>();
    if phases.is_empty() {
        "Pending".into()
    } else if phases.contains(&"Failed") {
        "Failed".into()
    } else if phases.iter().all(|phase| *phase == "Succeeded") {
        "Succeeded".into()
    } else {
        "Running".into()
    }
}

fn invoke_provider(
    config: &Config,
    providers: &[Manifest],
    capability: Capability,
    request: Value,
    scope: &Path,
) -> Result<ActionOutput> {
    let invocation = provider::invoke_capability(config, providers, capability, request)?;
    let mut value = invocation.value;
    if let Some(plan) = invocation.plan {
        let output = provider::capture_plan(&plan, scope, config.provider_timeout())?;
        value = if output.trim().is_empty() {
            json!({"status": "accepted"})
        } else {
            serde_json::from_str(&output).context("provider action output is not JSON")?
        };
    }
    Ok(ActionOutput {
        provider: invocation.provider,
        value,
    })
}

fn desired_state(resource: &Resource) -> &str {
    resource
        .spec
        .get("desiredState")
        .and_then(Value::as_str)
        .unwrap_or("running")
}

fn dependency_ready(store: &ControlStore, resource: &Resource) -> bool {
    resource
        .spec
        .get("dependsOn")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .all(|name| {
            store
                .resource(&ResourceKey {
                    kind: ResourceKind::Execution,
                    name: name.into(),
                })
                .is_some_and(|dependency| dependency.status.phase == "Succeeded")
        })
}

fn action_for(resource: &Resource) -> Option<Capability> {
    match resource.kind {
        ResourceKind::Execution => match desired_state(resource) {
            "cancelled" if resource.status.phase != "Cancelled" => {
                Some(Capability::ExecutionCancel)
            }
            _ if resource.status.phase == "Failed" => Some(Capability::ExecutionEnsure),
            _ if resource.status.observed_generation < resource.metadata.generation => {
                Some(Capability::ExecutionEnsure)
            }
            _ if matches!(resource.status.phase.as_str(), "Running" | "Pending" | "") => {
                Some(Capability::ExecutionObserve)
            }
            _ => None,
        },
        ResourceKind::Session
            if resource.status.observed_generation < resource.metadata.generation
                || matches!(resource.status.phase.as_str(), "Active" | "Failed" | "") =>
        {
            Some(Capability::SessionObserve)
        }
        _ => None,
    }
}

fn apply_action_output(
    store: &mut ControlStore,
    key: &ResourceKey,
    capability: Capability,
    output: ActionOutput,
) -> Result<bool> {
    let resource = store.resource_mut(key).context("resource disappeared")?;
    let before = resource.status.clone();
    resource.status.provider = Some(output.provider);
    resource.status.observed_generation = resource.metadata.generation;
    resource.status.phase = output
        .value
        .get("phase")
        .or_else(|| output.value.get("status"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| match capability {
            Capability::ExecutionEnsure => "Running".into(),
            Capability::ExecutionCancel => "Cancelled".into(),
            Capability::SessionObserve => "Active".into(),
            _ => resource.status.phase.clone(),
        });
    resource.status.external_ref = output
        .value
        .get("externalRef")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| resource.status.external_ref.clone());
    resource.status.message = output
        .value
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let outputs = output
        .value
        .get("outputs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut stored_outputs = Vec::new();
    for (name, value) in outputs {
        let media_type = if value.is_string() {
            "text/plain"
        } else {
            "application/json"
        };
        let body = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or(serde_json::to_string(&value)?);
        let reference = store.store_artifact(body, media_type.into());
        stored_outputs.push((name, reference));
    }
    let resource = store.resource_mut(key).context("resource disappeared")?;
    resource.status.outputs.extend(stored_outputs);
    Ok(resource.status != before)
}

fn binding_matches(resource: &Resource, event: &ControlEvent) -> bool {
    let event_types = resource
        .spec
        .get("eventTypes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let reasons = resource
        .spec
        .get("reasons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let subject_kinds = resource
        .spec
        .get("subjectKinds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    (event_types.is_empty() || event_types.contains(&event.event_type.as_str()))
        && (reasons.is_empty() || reasons.contains(&event.reason.as_str()))
        && (subject_kinds.is_empty()
            || subject_kinds.contains(&event.subject.kind.to_string().as_str()))
}

pub fn reconcile(
    config: &Config,
    scope: &Path,
    max_passes: usize,
    dry_run: bool,
) -> Result<ReconcileResult> {
    let scope = state::resolve_scope(scope)?;
    let providers = provider::discover(config)?;
    reconcile_with(
        &scope,
        max_passes.clamp(1, 128),
        dry_run,
        |capability, request| invoke_provider(config, &providers, capability, request, &scope),
    )
}

fn reconcile_with(
    scope: &Path,
    max_passes: usize,
    dry_run: bool,
    mut invoke: impl FnMut(Capability, Value) -> Result<ActionOutput>,
) -> Result<ReconcileResult> {
    let scope = state::resolve_scope(scope)?;
    let mut operation = |store: &mut ControlStore| {
        let mut actions = Vec::new();
        let mut attempted = BTreeSet::new();
        let mut attempted_events = BTreeSet::new();
        let mut passes = 0;
        for pass in 0..max_passes {
            passes = pass + 1;
            let mut changed = reconcile_runs(store)?;
            let levels = execution_levels(store)?;
            let mut ordered = store
                .resources
                .values()
                .filter(|resource| resource.kind == ResourceKind::Session)
                .map(Resource::key)
                .collect::<Vec<_>>();
            ordered.extend(levels.into_iter().flatten());
            for key in ordered {
                let resource = store
                    .resource(&key)
                    .context("resource disappeared")?
                    .clone();
                if resource.kind == ResourceKind::Execution && !dependency_ready(store, &resource) {
                    continue;
                }
                let Some(capability) = action_for(&resource) else {
                    continue;
                };
                if !attempted.insert((key.clone(), capability)) {
                    continue;
                }
                let request = provider_request(store, &scope, &resource, capability, None)?;
                let operation_id = operation_id(&resource, capability);
                if dry_run {
                    actions.push(ProviderAction {
                        operation_id,
                        capability,
                        resource: key,
                        provider: None,
                        changed: false,
                        error: None,
                    });
                    continue;
                }
                match invoke(capability, request) {
                    Ok(output) => {
                        let provider = Some(output.provider.clone());
                        let action_changed = apply_action_output(store, &key, capability, output)?;
                        if action_changed {
                            store.emit(
                                "Normal",
                                "Reconciled",
                                key.clone(),
                                format!("{key} reconciled through {capability}"),
                            );
                        }
                        changed |= action_changed;
                        actions.push(ProviderAction {
                            operation_id,
                            capability,
                            resource: key,
                            provider,
                            changed: action_changed,
                            error: None,
                        });
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        let resource = store.resource_mut(&key).context("resource disappeared")?;
                        resource.status.phase = "Failed".into();
                        resource.status.observed_generation = resource.metadata.generation;
                        resource.status.message = Some(message.clone());
                        store.emit("Warning", "ReconcileFailed", key.clone(), message.clone());
                        actions.push(ProviderAction {
                            operation_id,
                            capability,
                            resource: key,
                            provider: None,
                            changed: true,
                            error: Some(message),
                        });
                        changed = true;
                    }
                }
            }
            if !dry_run {
                changed |= deliver_events(
                    store,
                    &scope,
                    &mut invoke,
                    &mut actions,
                    &mut attempted_events,
                )?;
            }
            if !changed || dry_run {
                break;
            }
        }
        Ok(ReconcileResult {
            dry_run,
            passes,
            actions,
        })
    };
    if dry_run {
        let mut store = read_store(&scope)?;
        operation(&mut store)
    } else {
        update_store(&scope, operation)
    }
}

fn deliver_events(
    store: &mut ControlStore,
    scope: &Path,
    invoke: &mut impl FnMut(Capability, Value) -> Result<ActionOutput>,
    actions: &mut Vec<ProviderAction>,
    attempted: &mut BTreeSet<(String, u64)>,
) -> Result<bool> {
    let bindings = store
        .resources
        .values()
        .filter(|resource| resource.kind == ResourceKind::EventBinding)
        .cloned()
        .collect::<Vec<_>>();
    let events = store.events.clone();
    let mut changed = false;
    for binding in bindings {
        for event in events.iter().filter(|event| {
            !binding.status.delivered_events.contains(&event.sequence)
                && event.subject != binding.key()
                && binding_matches(&binding, event)
        }) {
            if !attempted.insert((binding.metadata.uid.clone(), event.sequence)) {
                continue;
            }
            let mut request = provider_request(
                store,
                scope,
                &binding,
                Capability::EventDeliver,
                Some(event),
            )?;
            let operation_id = format!(
                "{}-{}",
                operation_id(&binding, Capability::EventDeliver),
                event.sequence
            );
            request["operationId"] = Value::String(operation_id.clone());
            let key = binding.key();
            match invoke(Capability::EventDeliver, request) {
                Ok(output) => {
                    let resource = store
                        .resource_mut(&key)
                        .context("event binding disappeared")?;
                    resource.status.provider = Some(output.provider.clone());
                    resource.status.phase = "Active".into();
                    resource.status.observed_generation = resource.metadata.generation;
                    resource.status.delivered_events.push(event.sequence);
                    actions.push(ProviderAction {
                        operation_id,
                        capability: Capability::EventDeliver,
                        resource: key,
                        provider: Some(output.provider),
                        changed: true,
                        error: None,
                    });
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    {
                        let resource = store
                            .resource_mut(&key)
                            .context("event binding disappeared")?;
                        resource.status.phase = "Failed".into();
                        resource.status.observed_generation = resource.metadata.generation;
                        resource.status.message = Some(message.clone());
                    }
                    store.emit(
                        "Warning",
                        "EventDeliveryFailed",
                        key.clone(),
                        message.clone(),
                    );
                    actions.push(ProviderAction {
                        operation_id,
                        capability: Capability::EventDeliver,
                        resource: key,
                        provider: None,
                        changed: true,
                        error: Some(message),
                    });
                }
            }
            changed = true;
        }
    }
    Ok(changed)
}

pub fn logs(config: &Config, scope: &Path, name: &str) -> Result<String> {
    let scope = state::resolve_scope(scope)?;
    let store = read_store(&scope)?;
    let resource = store
        .resource(&ResourceKey {
            kind: ResourceKind::Execution,
            name: name.into(),
        })
        .with_context(|| format!("execution/{name} not found"))?;
    let providers = provider::discover(config)?;
    let request = provider_request(&store, &scope, resource, Capability::ExecutionLogs, None)?;
    let output = invoke_provider(
        config,
        &providers,
        Capability::ExecutionLogs,
        request,
        &scope,
    )?;
    Ok(output
        .value
        .get("logs")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn resource(kind: ResourceKind, name: &str, spec: Value) -> Resource {
        Resource {
            api_version: API_VERSION.into(),
            kind,
            metadata: ObjectMeta {
                name: name.into(),
                uid: String::new(),
                generation: 0,
                resource_version: 0,
                labels: BTreeMap::new(),
                field_owners: BTreeMap::new(),
                created_at: epoch(),
                updated_at: epoch(),
            },
            spec,
            status: ResourceStatus::default(),
        }
    }

    fn scope() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn dry_run_has_no_persistent_writes() {
        let scope = scope();
        let result = apply(
            scope.path(),
            vec![resource(
                ResourceKind::Workflow,
                "build",
                json!({"goal": "ship"}),
            )],
            "test",
            false,
            true,
        )
        .unwrap();

        assert_eq!(result.changes[0].action, ChangeAction::Create);
        assert!(!control_path(scope.path()).exists());
    }

    #[test]
    fn field_ownership_rejects_conflicting_manager() {
        let scope = scope();
        apply(
            scope.path(),
            vec![resource(
                ResourceKind::Workflow,
                "build",
                json!({"goal": "one"}),
            )],
            "first",
            false,
            false,
        )
        .unwrap();
        let error = apply(
            scope.path(),
            vec![resource(
                ResourceKind::Workflow,
                "build",
                json!({"goal": "two"}),
            )],
            "second",
            false,
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("owned by first"));
        assert_eq!(
            list(scope.path(), Some(ResourceKind::Workflow), Some("build")).unwrap()[0].spec["goal"],
            "one"
        );
    }

    #[test]
    fn artifacts_are_content_addressed_and_survive_reload() {
        let scope = scope();
        apply(
            scope.path(),
            vec![resource(
                ResourceKind::Artifact,
                "report",
                json!({"content": "verified", "mediaType": "text/plain"}),
            )],
            "test",
            false,
            false,
        )
        .unwrap();

        let first = list(scope.path(), Some(ResourceKind::Artifact), Some("report"))
            .unwrap()
            .remove(0);
        assert!(
            first.spec["digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(artifact_body(scope.path(), "report").unwrap(), "verified");
        let unchanged = apply(
            scope.path(),
            vec![resource(
                ResourceKind::Artifact,
                "report",
                json!({"content": "verified", "mediaType": "text/plain"}),
            )],
            "test",
            false,
            false,
        )
        .unwrap();
        assert_eq!(unchanged.changes[0].action, ChangeAction::Unchanged);
        let resolved = state::resolve_scope(scope.path()).unwrap();
        assert!(
            !fs::read_to_string(control_path(&resolved))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reconcile_is_level_ordered_and_idempotent() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(
                    ResourceKind::Execution,
                    "build",
                    json!({"desiredState": "running"}),
                ),
                resource(
                    ResourceKind::Execution,
                    "verify",
                    json!({"desiredState": "running", "dependsOn": ["build"]}),
                ),
            ],
            "test",
            false,
            false,
        )
        .unwrap();
        let mut calls = Vec::new();
        let first = reconcile_with(scope.path(), 8, false, |capability, request| {
            calls.push((
                capability,
                request["resource"]["metadata"]["name"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            ));
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Succeeded", "outputs": {"result": "ok"}}),
            })
        })
        .unwrap();
        let second = reconcile_with(scope.path(), 8, false, |_, _| {
            panic!("stable resources must not invoke providers")
        })
        .unwrap();

        assert_eq!(
            calls,
            vec![
                (Capability::ExecutionEnsure, "build".into()),
                (Capability::ExecutionEnsure, "verify".into())
            ]
        );
        assert_eq!(first.actions.len(), 2);
        assert!(second.actions.is_empty());
        assert_eq!(events(scope.path(), None, None, 0).unwrap().len(), 4);
        let stored = read_store(&state::resolve_scope(scope.path()).unwrap()).unwrap();
        assert_eq!(stored.artifacts.len(), 1);
        assert_eq!(stored.artifacts.values().next().unwrap().body, "ok");
    }

    #[test]
    fn workflow_run_materializes_stages_and_reports_completion() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(
                    ResourceKind::Workflow,
                    "release",
                    json!({"stages": [
                        {"name": "build"},
                        {"name": "verify", "dependsOn": ["build"], "inputs": {
                            "build": {"executionRef": "release-1-build", "output": "result"}
                        }}
                    ]}),
                ),
                resource(
                    ResourceKind::Run,
                    "release-1",
                    json!({"workflowRef": "release"}),
                ),
            ],
            "test",
            false,
            false,
        )
        .unwrap();
        let result = reconcile_with(scope.path(), 8, false, |_, _| {
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Succeeded", "outputs": {"result": "ok"}}),
            })
        })
        .unwrap();

        assert_eq!(result.actions.len(), 2);
        assert_eq!(
            list(scope.path(), Some(ResourceKind::Execution), None)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            list(scope.path(), Some(ResourceKind::Run), Some("release-1")).unwrap()[0]
                .status
                .phase,
            "Succeeded"
        );
    }

    #[test]
    fn reconcile_dry_run_never_invokes_or_persists() {
        let scope = scope();
        apply(
            scope.path(),
            vec![resource(
                ResourceKind::Execution,
                "build",
                json!({"desiredState": "running"}),
            )],
            "test",
            false,
            false,
        )
        .unwrap();
        let before = fs::read(control_path(&state::resolve_scope(scope.path()).unwrap())).unwrap();
        let result = reconcile_with(scope.path(), 8, true, |_, _| {
            panic!("dry-run must not invoke providers")
        })
        .unwrap();
        let after = fs::read(control_path(&state::resolve_scope(scope.path()).unwrap())).unwrap();

        assert_eq!(result.actions.len(), 1);
        assert_eq!(before, after);
    }

    #[test]
    fn event_delivery_is_exactly_once_per_binding_and_event() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(
                    ResourceKind::EventBinding,
                    "notify",
                    json!({"reasons": ["Created"]}),
                ),
                resource(ResourceKind::Workflow, "build", json!({"goal": "ship"})),
            ],
            "test",
            false,
            false,
        )
        .unwrap();
        let mut deliveries = 0;
        reconcile_with(scope.path(), 4, false, |capability, _| {
            assert_eq!(capability, Capability::EventDeliver);
            deliveries += 1;
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"status": "delivered"}),
            })
        })
        .unwrap();
        reconcile_with(scope.path(), 4, false, |_, _| {
            panic!("event must not be redelivered")
        })
        .unwrap();

        assert_eq!(deliveries, 1);
        let binding = list(
            scope.path(),
            Some(ResourceKind::EventBinding),
            Some("notify"),
        )
        .unwrap()
        .remove(0);
        assert_eq!(binding.status.delivered_events.len(), 1);
    }

    #[test]
    fn provider_failure_is_durable_and_triggers_a_binding() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(
                    ResourceKind::Execution,
                    "build",
                    json!({"desiredState": "running"}),
                ),
                resource(
                    ResourceKind::EventBinding,
                    "failure-hook",
                    json!({"reasons": ["ReconcileFailed"]}),
                ),
            ],
            "test",
            false,
            false,
        )
        .unwrap();
        let mut delivered = false;
        let result = reconcile_with(scope.path(), 4, false, |capability, _| match capability {
            Capability::ExecutionEnsure => Err(anyhow::anyhow!("provider unavailable")),
            Capability::EventDeliver => {
                delivered = true;
                Ok(ActionOutput {
                    provider: "hook".into(),
                    value: json!({"status": "delivered"}),
                })
            }
            _ => panic!("unexpected capability {capability}"),
        })
        .unwrap();

        assert!(delivered);
        assert!(result.actions.iter().any(|action| action.error.is_some()));
        assert_eq!(
            list(scope.path(), Some(ResourceKind::Execution), Some("build")).unwrap()[0]
                .status
                .phase,
            "Failed"
        );
        assert!(
            events(scope.path(), None, None, 0)
                .unwrap()
                .iter()
                .any(|event| event.reason == "ReconcileFailed")
        );
        let failed_operation = result
            .actions
            .iter()
            .find(|action| action.capability == Capability::ExecutionEnsure)
            .unwrap()
            .operation_id
            .clone();
        let retry = reconcile_with(scope.path(), 4, false, |capability, _| {
            assert_eq!(capability, Capability::ExecutionEnsure);
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Succeeded"}),
            })
        })
        .unwrap();
        assert_eq!(retry.actions[0].operation_id, failed_operation);
        assert_eq!(
            list(scope.path(), Some(ResourceKind::Execution), Some("build")).unwrap()[0]
                .status
                .phase,
            "Succeeded"
        );
    }

    #[test]
    fn event_watch_stops_at_its_bound() {
        let scope = scope();
        let started = Instant::now();
        let delivered = watch_events(
            scope.path(),
            None,
            None,
            0,
            1,
            Duration::from_millis(5),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(delivered, 0);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
