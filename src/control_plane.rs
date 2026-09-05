use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

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
    let parent = prepare_state_directory(&target)?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(store)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .context("create private temporary control-plane state")?;
    file.write_all(&bytes)
        .context("write temporary control-plane state")?;
    file.sync_all()
        .context("sync temporary control-plane state")?;
    fs::rename(&temporary, &target).context("commit control-plane state")?;
    Ok(())
}

fn prepare_state_directory(target: &Path) -> Result<&Path> {
    let parent = target
        .parent()
        .context("control-plane state has no parent")?;
    if let Some(orc) = parent.parent() {
        fs::create_dir_all(orc).context("create Orc state directory")?;
        set_private_directory_permissions(orc)?;
    }
    fs::create_dir_all(parent).context("create control-plane state directory")?;
    set_private_directory_permissions(parent)?;
    Ok(parent)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("secure state directory {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn update_store<T>(
    scope: &Path,
    operation: impl FnOnce(&mut ControlStore) -> Result<T>,
) -> Result<T> {
    let target = control_path(scope);
    prepare_state_directory(&target)?;
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
    let spec = resource.spec.as_object().context("validated object spec")?;
    match resource.kind {
        ResourceKind::Workflow => validate_workflow_spec(resource, spec)?,
        ResourceKind::Run => {
            required_nonempty_string(spec, "workflowRef", &resource.key().to_string())?;
            if spec
                .get("parameters")
                .is_some_and(|parameters| !parameters.is_object())
            {
                bail!("{} spec.parameters must be an object", resource.key());
            }
        }
        ResourceKind::Session => validate_provider_inputs(resource, spec)?,
        ResourceKind::Execution => validate_execution_spec(resource, spec)?,
        ResourceKind::EventBinding => validate_event_binding_spec(resource, spec)?,
        ResourceKind::Artifact => validate_artifact_spec(resource, spec)?,
    }
    Ok(())
}

fn required_nonempty_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{context} spec.{field} must be a nonempty string"))
}

fn validate_optional_nonempty_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<()> {
    if object.contains_key(field) {
        required_nonempty_string(object, field, context)?;
    }
    Ok(())
}

fn validate_string_array(
    object: &serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<Vec<String>> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .with_context(|| format!("{context} spec.{field} must be an array of strings"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .with_context(|| format!("{context} spec.{field} entries must be nonempty strings"))
        })
        .collect()
}

fn validate_inputs(object: &serde_json::Map<String, Value>, context: &str) -> Result<()> {
    let Some(inputs) = object.get("inputs") else {
        return Ok(());
    };
    let inputs = inputs
        .as_object()
        .with_context(|| format!("{context} spec.inputs must be an object"))?;
    for (name, value) in inputs {
        if name.trim().is_empty() {
            bail!("{context} spec.inputs keys must be nonempty");
        }
        let Some(input) = value.as_object() else {
            continue;
        };
        let has_artifact = input.contains_key("artifactRef");
        let has_execution = input.contains_key("executionRef");
        if has_artifact && has_execution {
            bail!("{context} input {name} cannot combine artifactRef and executionRef");
        }
        if has_artifact {
            required_nonempty_string(input, "artifactRef", context)?;
            if input.contains_key("output") {
                bail!("{context} artifact input {name} cannot define output");
            }
        }
        if has_execution {
            required_nonempty_string(input, "executionRef", context)?;
            required_nonempty_string(input, "output", context)?;
        } else if input.contains_key("output") {
            bail!("{context} input {name} defines output without executionRef");
        }
    }
    Ok(())
}

fn validate_provider_inputs(
    resource: &Resource,
    object: &serde_json::Map<String, Value>,
) -> Result<()> {
    let context = resource.key().to_string();
    validate_optional_nonempty_string(object, "provider", &context)?;
    validate_inputs(object, &context)?;
    validate_actions(object, &context)
}

fn validate_execution_fields(object: &serde_json::Map<String, Value>, context: &str) -> Result<()> {
    validate_optional_nonempty_string(object, "provider", context)?;
    validate_inputs(object, context)?;
    validate_actions(object, context)?;
    validate_string_array(object, "dependsOn", context)?;
    if let Some(desired_state) = object.get("desiredState") {
        match desired_state.as_str() {
            Some("running" | "cancelled") => {}
            _ => bail!("{context} spec.desiredState must be running or cancelled"),
        }
    }
    Ok(())
}

fn validate_actions(object: &serde_json::Map<String, Value>, context: &str) -> Result<()> {
    let Some(actions) = object.get("actions") else {
        return Ok(());
    };
    let actions = actions
        .as_object()
        .with_context(|| format!("{context} spec.actions must be an object"))?;
    for (capability, action) in actions {
        if capability.trim().is_empty() {
            bail!("{context} spec.actions keys must be nonempty");
        }
        let action = action
            .as_object()
            .with_context(|| format!("{context} spec.actions.{capability} must be an object"))?;
        let command = action
            .get("command")
            .and_then(Value::as_array)
            .filter(|command| !command.is_empty())
            .with_context(|| {
                format!(
                    "{context} spec.actions.{capability}.command must be a nonempty string array"
                )
            })?;
        if command
            .iter()
            .any(|argument| argument.as_str().is_none_or(str::is_empty))
        {
            bail!("{context} spec.actions.{capability}.command must be a nonempty string array");
        }
        validate_optional_nonempty_string(action, "cwd", context)?;
        if let Some(environment) = action.get("environment") {
            let environment = environment.as_object().with_context(|| {
                format!("{context} spec.actions.{capability}.environment must be an object")
            })?;
            if environment.values().any(|value| !value.is_string()) {
                bail!("{context} spec.actions.{capability}.environment values must be strings");
            }
        }
    }
    Ok(())
}

fn validate_execution_spec(
    resource: &Resource,
    object: &serde_json::Map<String, Value>,
) -> Result<()> {
    validate_execution_fields(object, &resource.key().to_string())
}

fn validate_workflow_spec(
    resource: &Resource,
    object: &serde_json::Map<String, Value>,
) -> Result<()> {
    let context = resource.key().to_string();
    let stages = object
        .get("stages")
        .and_then(Value::as_array)
        .with_context(|| format!("{context} spec.stages must be an array"))?;
    let mut names = BTreeSet::new();
    let mut dependencies = Vec::new();
    for (index, stage) in stages.iter().enumerate() {
        let stage = stage
            .as_object()
            .with_context(|| format!("{context} stage {index} must be an object"))?;
        let stage_context = format!("{context} stage {index}");
        let name = required_nonempty_string(stage, "name", &stage_context)?.to_owned();
        if !names.insert(name.clone()) {
            bail!("{context} has duplicate stage {name}");
        }
        validate_execution_fields(stage, &stage_context)?;
        dependencies.push((
            name,
            validate_string_array(stage, "dependsOn", &stage_context)?,
        ));
    }
    for (stage, stage_dependencies) in dependencies {
        for dependency in stage_dependencies {
            if dependency == stage {
                bail!("{context} stage {stage} cannot depend on itself");
            }
            if !names.contains(&dependency) {
                bail!("{context} stage {stage} depends on missing stage {dependency}");
            }
        }
    }
    Ok(())
}

fn validate_event_binding_spec(
    resource: &Resource,
    object: &serde_json::Map<String, Value>,
) -> Result<()> {
    let context = resource.key().to_string();
    validate_provider_inputs(resource, object)?;
    for event_type in validate_string_array(object, "eventTypes", &context)? {
        if !matches!(event_type.as_str(), "Normal" | "Warning") {
            bail!("{context} spec.eventTypes entries must be Normal or Warning");
        }
    }
    validate_string_array(object, "reasons", &context)?;
    for kind in validate_string_array(object, "subjectKinds", &context)? {
        ResourceKind::from_str(&kind)
            .with_context(|| format!("{context} spec.subjectKinds contains {kind}"))?;
    }
    Ok(())
}

fn validate_artifact_spec(
    resource: &Resource,
    object: &serde_json::Map<String, Value>,
) -> Result<()> {
    let context = resource.key().to_string();
    validate_optional_nonempty_string(object, "mediaType", &context)?;
    if object.contains_key("content") {
        return Ok(());
    }
    let digest = required_nonempty_string(object, "digest", &context)?;
    let valid_digest = digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64 && hex.chars().all(|character| character.is_ascii_hexdigit())
    });
    if !valid_digest {
        bail!("{context} spec.digest must be a sha256 digest");
    }
    if object.get("size").and_then(Value::as_u64).is_none() {
        bail!("{context} spec.size must be a nonnegative integer");
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
        validate_resource(resource)?;
        validate_artifact_body(&store, resource)?;
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
    for resource in &resources {
        validate_resource(resource)?;
    }
    let scope = state::resolve_scope(scope)?;
    let operation = |store: &mut ControlStore| {
        let mut changes = Vec::new();
        for mut incoming in resources {
            validate_artifact_body(store, &incoming)?;
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

fn validate_artifact_body(store: &ControlStore, resource: &Resource) -> Result<()> {
    if resource.kind != ResourceKind::Artifact || resource.spec.get("content").is_some() {
        return Ok(());
    }
    let digest = resource
        .spec
        .get("digest")
        .and_then(Value::as_str)
        .context("artifact digest is required when content is absent")?;
    if !store.artifacts.contains_key(digest) {
        bail!(
            "{} references missing artifact body {digest}",
            resource.key()
        );
    }
    Ok(())
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
        if kind == ResourceKind::Workflow {
            let dependents = store
                .resources
                .values()
                .filter(|resource| resource.kind == ResourceKind::Run)
                .filter(|resource| {
                    resource.spec.get("workflowRef").and_then(Value::as_str) == Some(name)
                })
                .map(|resource| resource.metadata.name.clone())
                .collect::<Vec<_>>();
            if !dependents.is_empty() {
                bail!("{key} is referenced by runs/{}", dependents.join(", runs/"));
            }
        }
        if kind == ResourceKind::Run {
            let children = store
                .resources
                .values()
                .filter(|resource| generated_for_run(resource, name))
                .map(Resource::key)
                .collect::<Vec<_>>();
            if children.iter().any(|child| {
                store
                    .resource(child)
                    .is_some_and(|resource| !execution_is_terminal(resource))
            }) {
                let now = Utc::now();
                let mut retired = Vec::new();
                for child in &children {
                    if store
                        .resource(child)
                        .is_some_and(|resource| desired_state(resource) != "cancelled")
                    {
                        store.resource_version += 1;
                        let resource_version = store.resource_version;
                        let execution = store
                            .resource_mut(child)
                            .context("generated execution disappeared")?;
                        execution.spec["desiredState"] = Value::String("cancelled".into());
                        execution.metadata.generation += 1;
                        execution.metadata.resource_version = resource_version;
                        execution.metadata.updated_at = now;
                        retired.push(child.clone());
                    }
                }
                let run = store.resource_mut(&key).context("run disappeared")?;
                if desired_state(run) != "cancelled" {
                    store.resource_version += 1;
                    let resource_version = store.resource_version;
                    let run = store.resource_mut(&key).context("run disappeared")?;
                    run.spec["desiredState"] = Value::String("cancelled".into());
                    run.metadata.generation += 1;
                    run.metadata.resource_version = resource_version;
                    run.metadata.updated_at = now;
                }
                for child in retired {
                    store.emit(
                        "Normal",
                        "Retired",
                        child.clone(),
                        format!("{child} is stopping before {key} deletion"),
                    );
                }
                store.emit(
                    "Normal",
                    "DeletionRequested",
                    key.clone(),
                    format!("{key} deletion is waiting for owned executions"),
                );
                return Ok(ResourceChange {
                    resource: key.clone(),
                    action: ChangeAction::Delete,
                    paths: vec!["spec.desiredState".into()],
                });
            }
            for child in children {
                store.resources.remove(&child.to_string());
                store.resource_version += 1;
                store.emit(
                    "Normal",
                    "Deleted",
                    child.clone(),
                    format!("{child} deleted with {key}"),
                );
            }
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

fn generated_for_run(resource: &Resource, run_name: &str) -> bool {
    resource.kind == ResourceKind::Execution
        && resource.spec.get("runRef").and_then(Value::as_str) == Some(run_name)
        && resource
            .metadata
            .field_owners
            .get("spec")
            .is_some_and(|owner| owner == "orc-reconciler")
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
        if desired_state(&run) == "cancelled" {
            let run_key = run.key();
            let children = store
                .resources
                .values()
                .filter(|resource| generated_for_run(resource, &run.metadata.name))
                .map(Resource::key)
                .collect::<Vec<_>>();
            for child in &children {
                if store
                    .resource(child)
                    .is_some_and(|resource| desired_state(resource) != "cancelled")
                {
                    let terminal = store.resource(child).is_some_and(execution_is_terminal);
                    store.resource_version += 1;
                    let resource_version = store.resource_version;
                    let execution = store
                        .resource_mut(child)
                        .context("generated execution disappeared")?;
                    execution.spec["desiredState"] = Value::String("cancelled".into());
                    execution.metadata.generation += 1;
                    execution.metadata.resource_version = resource_version;
                    execution.metadata.updated_at = Utc::now();
                    if terminal {
                        execution.status.observed_generation = execution.metadata.generation;
                    }
                    store.emit(
                        "Normal",
                        "Retired",
                        child.clone(),
                        format!("{child} is stopping before {run_key} deletion"),
                    );
                    changed = true;
                }
            }
            if children
                .iter()
                .all(|child| store.resource(child).is_some_and(execution_is_terminal))
            {
                for child in children {
                    store.resources.remove(&child.to_string());
                    store.resource_version += 1;
                    store.emit(
                        "Normal",
                        "Deleted",
                        child.clone(),
                        format!("{child} deleted with {run_key}"),
                    );
                }
                store.resources.remove(&run_key.to_string());
                store.resource_version += 1;
                store.emit(
                    "Normal",
                    "Deleted",
                    run_key.clone(),
                    format!("{run_key} deleted after owned executions stopped"),
                );
                changed = true;
            }
            continue;
        }
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
        let mut desired_executions = BTreeSet::new();
        for stage in stages {
            let stage_name = stage["name"]
                .as_str()
                .context("workflow stage name is required")?;
            let execution_name = format!("{}-{stage_name}", run.metadata.name);
            let key = ResourceKey {
                kind: ResourceKind::Execution,
                name: execution_name.clone(),
            };
            desired_executions.insert(execution_name.clone());
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
            let desired_spec = Value::Object(spec);
            match store.resource(&key).cloned() {
                None => {
                    store.resource_version += 1;
                    let mut execution =
                        generated_resource(ResourceKind::Execution, execution_name, desired_spec);
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
                Some(existing) if existing.spec != desired_spec => {
                    let owned_by_reconciler = existing
                        .metadata
                        .field_owners
                        .get("spec")
                        .is_some_and(|owner| owner == "orc-reconciler");
                    let belongs_to_run = existing.spec.get("runRef").and_then(Value::as_str)
                        == Some(run.metadata.name.as_str());
                    if !owned_by_reconciler || !belongs_to_run {
                        bail!("{key} conflicts with a non-generated execution");
                    }
                    store.resource_version += 1;
                    let resource_version = store.resource_version;
                    let execution = store
                        .resource_mut(&key)
                        .context("generated execution disappeared")?;
                    execution.spec = desired_spec;
                    execution.metadata.generation += 1;
                    execution.metadata.resource_version = resource_version;
                    execution.metadata.updated_at = Utc::now();
                    store.emit(
                        "Normal",
                        "Configured",
                        key.clone(),
                        format!("{key} updated from {}", workflow.key()),
                    );
                    changed = true;
                }
                Some(_) => {}
            }
        }
        let retired = store
            .resources
            .values()
            .filter(|resource| generated_for_run(resource, &run.metadata.name))
            .filter(|resource| !desired_executions.contains(&resource.metadata.name))
            .filter(|resource| desired_state(resource) != "cancelled")
            .map(Resource::key)
            .collect::<Vec<_>>();
        for key in retired {
            let terminal = store.resource(&key).is_some_and(execution_is_terminal);
            store.resource_version += 1;
            let resource_version = store.resource_version;
            let execution = store
                .resource_mut(&key)
                .context("generated execution disappeared")?;
            execution.spec["desiredState"] = Value::String("cancelled".into());
            execution.metadata.generation += 1;
            execution.metadata.resource_version = resource_version;
            execution.metadata.updated_at = Utc::now();
            if terminal {
                execution.status.observed_generation = execution.metadata.generation;
            }
            store.emit(
                "Normal",
                "Pruned",
                key.clone(),
                format!("{key} retired after removal from {}", workflow.key()),
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
        .filter(|resource| generated_for_run(resource, run_name))
        .filter(|resource| desired_state(resource) != "cancelled")
        .map(|execution| execution.status.phase.as_str())
        .collect::<Vec<_>>();
    if phases.is_empty() {
        "Succeeded".into()
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
        value = parse_action_observation(&output)?;
    }
    Ok(ActionOutput {
        provider: invocation.provider,
        value,
    })
}

fn parse_action_observation(output: &str) -> Result<Value> {
    if output.trim().is_empty() {
        bail!("provider action command returned no observation");
    }
    serde_json::from_str(output).context("provider action output is not JSON")
}

fn desired_state(resource: &Resource) -> &str {
    resource
        .spec
        .get("desiredState")
        .and_then(Value::as_str)
        .unwrap_or("running")
}

fn execution_is_terminal(resource: &Resource) -> bool {
    matches!(
        resource.status.phase.as_str(),
        "Succeeded" | "Failed" | "Cancelled"
    )
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
            "cancelled" if execution_is_terminal(resource) => None,
            "cancelled" => Some(Capability::ExecutionCancel),
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
                        if capability != Capability::ExecutionCancel {
                            resource.status.phase = "Failed".into();
                            resource.status.observed_generation = resource.metadata.generation;
                        }
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
                && event.reason != "EventDeliveryFailed"
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
                json!({"goal": "ship", "stages": []}),
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
    fn invalid_execution_dependencies_are_rejected_before_state_changes() {
        let scope = scope();
        let invalid = resource(
            ResourceKind::Execution,
            "verify",
            json!({"dependsOn": "build", "desiredState": "running"}),
        );

        let error = apply(scope.path(), vec![invalid.clone()], "test", false, true).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("spec.dependsOn must be an array of strings")
        );
        assert!(diff(scope.path(), &[invalid]).is_err());
        assert!(!control_path(scope.path()).exists());
    }

    #[cfg(unix)]
    #[test]
    fn control_state_remains_private_across_atomic_replacement() {
        let scope = scope();
        for goal in ["one", "two"] {
            apply(
                scope.path(),
                vec![resource(
                    ResourceKind::Workflow,
                    "build",
                    json!({"goal": goal, "stages": []}),
                )],
                "test",
                false,
                false,
            )
            .unwrap();
            let path = control_path(&state::resolve_scope(scope.path()).unwrap());
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap().parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn field_ownership_rejects_conflicting_manager() {
        let scope = scope();
        apply(
            scope.path(),
            vec![resource(
                ResourceKind::Workflow,
                "build",
                json!({"goal": "one", "stages": []}),
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
                json!({"goal": "two", "stages": []}),
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
    fn artifact_digest_cannot_reference_a_missing_body() {
        let scope = scope();
        let dangling = resource(
            ResourceKind::Artifact,
            "report",
            json!({
                "digest": format!("sha256:{}", "0".repeat(64)),
                "size": 8,
                "mediaType": "text/plain"
            }),
        );

        let error = apply(scope.path(), vec![dangling.clone()], "test", false, false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("references missing artifact body")
        );
        assert!(diff(scope.path(), &[dangling]).is_err());
        assert!(!control_path(scope.path()).exists());
    }

    #[test]
    fn successful_action_command_requires_an_observation() {
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
        let plan = provider::CommandPlan {
            version: "orc.command/v1".into(),
            command: vec!["/bin/sh".into(), "-c".into(), ":".into()],
            cwd: None,
            environment: BTreeMap::new(),
            success_codes: vec![0],
        };

        let result = reconcile_with(scope.path(), 2, false, |capability, _| {
            assert_eq!(capability, Capability::ExecutionEnsure);
            let output = provider::capture_plan(&plan, scope.path(), Duration::from_secs(1))?;
            Ok(ActionOutput {
                provider: "fake".into(),
                value: parse_action_observation(&output)?,
            })
        })
        .unwrap();

        assert_eq!(result.actions.len(), 1);
        assert!(
            result.actions[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("returned no observation"))
        );
        assert_eq!(
            list(scope.path(), Some(ResourceKind::Execution), Some("build")).unwrap()[0]
                .status
                .phase,
            "Failed"
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
    fn referenced_workflow_cannot_be_deleted() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(ResourceKind::Workflow, "release", json!({"stages": []})),
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

        let error = delete(scope.path(), ResourceKind::Workflow, "release", false).unwrap_err();

        assert!(error.to_string().contains("referenced by runs/release-1"));
        assert!(list(scope.path(), Some(ResourceKind::Workflow), Some("release")).is_ok());
    }

    #[test]
    fn run_deletion_stops_and_removes_owned_executions() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(
                    ResourceKind::Workflow,
                    "release",
                    json!({"stages": [{"name": "build"}]}),
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
        reconcile_with(scope.path(), 1, false, |capability, _| {
            assert_eq!(capability, Capability::ExecutionEnsure);
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Running"}),
            })
        })
        .unwrap();

        delete(scope.path(), ResourceKind::Run, "release-1", false).unwrap();

        let run = list(scope.path(), Some(ResourceKind::Run), Some("release-1"))
            .unwrap()
            .remove(0);
        let execution = list(
            scope.path(),
            Some(ResourceKind::Execution),
            Some("release-1-build"),
        )
        .unwrap()
        .remove(0);
        assert_eq!(run.spec["desiredState"], "cancelled");
        assert_eq!(execution.spec["desiredState"], "cancelled");

        let result = reconcile_with(scope.path(), 4, false, |capability, _| {
            assert_eq!(capability, Capability::ExecutionCancel);
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Cancelled"}),
            })
        })
        .unwrap();

        assert_eq!(result.actions.len(), 1);
        assert!(list(scope.path(), Some(ResourceKind::Run), Some("release-1")).is_err());
        assert!(
            list(
                scope.path(),
                Some(ResourceKind::Execution),
                Some("release-1-build")
            )
            .is_err()
        );
    }

    #[test]
    fn workflow_update_reconfigures_existing_execution() {
        let scope = scope();
        let workflow = |provider: &str, command: &str, depends_on: Value| {
            resource(
                ResourceKind::Workflow,
                "release",
                json!({"stages": [
                    {"name": "prepare", "provider": "v1", "command": ["prepare"]},
                    {
                        "name": "build",
                        "provider": provider,
                        "command": [command],
                        "dependsOn": depends_on
                    }
                ]}),
            )
        };
        apply(
            scope.path(),
            vec![
                workflow("v1", "build-v1", json!([])),
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
        reconcile_with(scope.path(), 8, false, |_, _| {
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Succeeded"}),
            })
        })
        .unwrap();
        let before = list(
            scope.path(),
            Some(ResourceKind::Execution),
            Some("release-1-build"),
        )
        .unwrap()
        .remove(0);

        apply(
            scope.path(),
            vec![workflow("v2", "build-v2", json!(["prepare"]))],
            "test",
            false,
            false,
        )
        .unwrap();
        let mut requests = Vec::new();
        let reconciled = reconcile_with(scope.path(), 8, false, |capability, request| {
            requests.push((capability, request));
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Succeeded"}),
            })
        })
        .unwrap();

        assert_eq!(reconciled.actions.len(), 1);
        assert_eq!(
            reconciled.actions[0].capability,
            Capability::ExecutionEnsure
        );
        assert_eq!(requests.len(), 1);
        let requested = &requests[0].1["resource"];
        assert_eq!(requested["status"]["phase"], "Succeeded");
        assert_eq!(requested["status"]["observedGeneration"], 1);
        assert_eq!(requested["spec"]["provider"], "v2");
        assert_eq!(requested["spec"]["command"], json!(["build-v2"]));
        assert_eq!(requested["spec"]["dependsOn"], json!(["release-1-prepare"]));

        let after = list(
            scope.path(),
            Some(ResourceKind::Execution),
            Some("release-1-build"),
        )
        .unwrap()
        .remove(0);
        assert_eq!(after.metadata.generation, before.metadata.generation + 1);
        assert!(after.metadata.resource_version > before.metadata.resource_version);
        assert_eq!(after.metadata.field_owners, before.metadata.field_owners);
        assert_eq!(after.status.phase, "Succeeded");
        assert_eq!(after.status.observed_generation, after.metadata.generation);

        let stable = reconcile_with(scope.path(), 8, false, |_, _| {
            panic!("unchanged generated execution must not rerun")
        })
        .unwrap();
        assert!(stable.actions.is_empty());
    }

    #[test]
    fn removed_completed_stage_is_cancelled_and_retained_for_audit() {
        let scope = scope();
        let workflow =
            |stages: Value| resource(ResourceKind::Workflow, "release", json!({"stages": stages}));
        apply(
            scope.path(),
            vec![
                workflow(json!([{"name": "build", "provider": "fake"}])),
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
        reconcile_with(scope.path(), 8, false, |_, _| {
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Succeeded"}),
            })
        })
        .unwrap();
        let before = list(
            scope.path(),
            Some(ResourceKind::Execution),
            Some("release-1-build"),
        )
        .unwrap()
        .remove(0);

        apply(
            scope.path(),
            vec![workflow(json!([]))],
            "test",
            false,
            false,
        )
        .unwrap();
        let reconciled = reconcile_with(scope.path(), 8, false, |_, _| {
            panic!("a completed one-shot stage must not require cancellation")
        })
        .unwrap();

        assert!(reconciled.actions.is_empty());
        let after = list(
            scope.path(),
            Some(ResourceKind::Execution),
            Some("release-1-build"),
        )
        .unwrap()
        .remove(0);
        assert_eq!(after.spec["desiredState"], "cancelled");
        assert_eq!(after.status.phase, "Succeeded");
        assert_eq!(after.metadata.generation, before.metadata.generation + 1);
        assert_eq!(after.status.observed_generation, after.metadata.generation);
        assert_eq!(after.metadata.field_owners, before.metadata.field_owners);
        assert_eq!(
            list(scope.path(), Some(ResourceKind::Run), Some("release-1")).unwrap()[0]
                .status
                .phase,
            "Succeeded"
        );
        assert!(
            events(
                scope.path(),
                Some(ResourceKind::Execution),
                Some("release-1-build"),
                0
            )
            .unwrap()
            .iter()
            .any(|event| event.reason == "Pruned")
        );
    }

    #[test]
    fn removed_failed_stage_preserves_its_terminal_outcome() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(
                    ResourceKind::Workflow,
                    "release",
                    json!({"stages": [{"name": "build"}]}),
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
        reconcile_with(scope.path(), 1, false, |_, _| {
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Failed", "message": "build failed"}),
            })
        })
        .unwrap();
        apply(
            scope.path(),
            vec![resource(
                ResourceKind::Workflow,
                "release",
                json!({"stages": []}),
            )],
            "test",
            false,
            false,
        )
        .unwrap();
        let reconciled = reconcile_with(scope.path(), 8, false, |_, _| {
            panic!("failed terminal work must not require cancellation")
        })
        .unwrap();

        assert!(reconciled.actions.is_empty());
        let execution = list(
            scope.path(),
            Some(ResourceKind::Execution),
            Some("release-1-build"),
        )
        .unwrap()
        .remove(0);
        assert_eq!(execution.spec["desiredState"], "cancelled");
        assert_eq!(execution.status.phase, "Failed");
        assert_eq!(
            execution.status.observed_generation,
            execution.metadata.generation
        );
    }

    #[test]
    fn removed_running_stage_is_cancelled_but_external_execution_is_preserved() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(
                    ResourceKind::Workflow,
                    "release",
                    json!({"stages": [{"name": "build"}]}),
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
        reconcile_with(scope.path(), 1, false, |capability, _| {
            assert_eq!(capability, Capability::ExecutionEnsure);
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": "Running"}),
            })
        })
        .unwrap();
        apply(
            scope.path(),
            vec![resource(
                ResourceKind::Execution,
                "external",
                json!({
                    "runRef": "release-1",
                    "desiredState": "running"
                }),
            )],
            "operator",
            false,
            false,
        )
        .unwrap();
        let external_before = list(
            scope.path(),
            Some(ResourceKind::Execution),
            Some("external"),
        )
        .unwrap()
        .remove(0);
        apply(
            scope.path(),
            vec![resource(
                ResourceKind::Workflow,
                "release",
                json!({"stages": []}),
            )],
            "test",
            false,
            false,
        )
        .unwrap();
        let mut cancelled = Vec::new();
        reconcile_with(scope.path(), 8, false, |capability, request| {
            let name = request["resource"]["metadata"]["name"].as_str().unwrap();
            let phase = match (name, capability) {
                ("external", Capability::ExecutionEnsure) => "Succeeded",
                ("release-1-build", Capability::ExecutionCancel) => {
                    cancelled.push(name.to_owned());
                    "Cancelled"
                }
                _ => panic!("unexpected action {capability} for {name}"),
            };
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"phase": phase}),
            })
        })
        .unwrap();

        assert_eq!(cancelled, vec!["release-1-build"]);
        let generated = list(
            scope.path(),
            Some(ResourceKind::Execution),
            Some("release-1-build"),
        )
        .unwrap()
        .remove(0);
        assert_eq!(generated.status.phase, "Cancelled");
        let external_after = list(
            scope.path(),
            Some(ResourceKind::Execution),
            Some("external"),
        )
        .unwrap()
        .remove(0);
        assert_eq!(
            external_after.metadata.generation,
            external_before.metadata.generation
        );
        assert_eq!(external_after.spec, external_before.spec);
        assert_eq!(external_after.status.phase, "Succeeded");
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
    fn event_delivery_stops_after_provider_acknowledgement() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(
                    ResourceKind::EventBinding,
                    "notify",
                    json!({"reasons": ["Created"]}),
                ),
                resource(
                    ResourceKind::Workflow,
                    "build",
                    json!({"goal": "ship", "stages": []}),
                ),
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
    fn event_delivery_retries_with_a_stable_operation_id() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(
                    ResourceKind::EventBinding,
                    "notify",
                    json!({"reasons": ["Created"], "subjectKinds": ["Workflow"]}),
                ),
                resource(
                    ResourceKind::Workflow,
                    "build",
                    json!({"goal": "ship", "stages": []}),
                ),
            ],
            "test",
            false,
            false,
        )
        .unwrap();

        let mut operation_ids = Vec::new();
        let first = reconcile_with(scope.path(), 4, false, |capability, request| {
            assert_eq!(capability, Capability::EventDeliver);
            operation_ids.push(request["operationId"].as_str().unwrap().to_owned());
            Err(anyhow::anyhow!(
                "provider performed its side effect, then returned invalid JSON"
            ))
        })
        .unwrap();
        assert_eq!(first.actions.len(), 1);
        assert!(first.actions[0].error.is_some());

        let second = reconcile_with(scope.path(), 4, false, |capability, request| {
            assert_eq!(capability, Capability::EventDeliver);
            operation_ids.push(request["operationId"].as_str().unwrap().to_owned());
            Ok(ActionOutput {
                provider: "fake".into(),
                value: json!({"status": "delivered"}),
            })
        })
        .unwrap();
        assert_eq!(second.actions.len(), 1);
        assert_eq!(operation_ids.len(), 2);
        assert_eq!(operation_ids[0], operation_ids[1]);
    }

    #[test]
    fn failed_event_bindings_do_not_deliver_each_others_failures() {
        let scope = scope();
        apply(
            scope.path(),
            vec![
                resource(ResourceKind::EventBinding, "first", json!({})),
                resource(ResourceKind::EventBinding, "second", json!({})),
                resource(
                    ResourceKind::Workflow,
                    "build",
                    json!({"goal": "ship", "stages": []}),
                ),
            ],
            "test",
            false,
            false,
        )
        .unwrap();

        let result = reconcile_with(scope.path(), 8, false, |capability, request| {
            assert_eq!(capability, Capability::EventDeliver);
            assert_ne!(request["event"]["reason"], "EventDeliveryFailed");
            Err(anyhow::anyhow!("hook unavailable"))
        })
        .unwrap();

        assert_eq!(result.passes, 2);
        assert_eq!(result.actions.len(), 4);
        assert_eq!(events(scope.path(), None, None, 0).unwrap().len(), 7);
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
