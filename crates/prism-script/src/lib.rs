//! Language-neutral scripting protocol boundary.
//!
//! Protocol DTOs are deliberately separate from domain and command types.
//! Conversion is the validation boundary; serialized input never mutates a
//! project directly.

use prism_commands::{Command, CommandEngine};
use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId};
use prism_project::{Project, ProxySourceFingerprint, TrackKind};
use prism_recipe::{RecipeError, RecipeLanguage, RecipeManifest, RecipePermission};
use prism_timeline::RationalTime;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
use uuid::Uuid;

/// First supported scripting protocol version.
pub const API_VERSION: &str = "1";

/// Version of the packaged runtime-bundle manifest understood by this build.
pub const RUNTIME_BUNDLE_API_VERSION: u32 = 1;

/// A reviewed set of capabilities requested by a script and granted by its host.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionGrantSet {
    /// Capabilities declared by the script or recipe.
    pub requested: Vec<RecipePermission>,
    /// Explicit host grants. Every entry must also appear in `requested`.
    pub granted: Vec<RecipePermission>,
}

impl PermissionGrantSet {
    /// Validates and deterministically normalizes a permission review.
    ///
    /// # Errors
    ///
    /// Returns [`PermissionGrantError::UndeclaredGrant`] if a grant was not
    /// requested by the script.
    pub fn normalize(mut self) -> Result<Self, PermissionGrantError> {
        self.requested
            .sort_by_key(|permission| permission_key(*permission));
        self.requested.dedup();
        self.granted
            .sort_by_key(|permission| permission_key(*permission));
        self.granted.dedup();
        if let Some(permission) = self
            .granted
            .iter()
            .find(|permission| !self.requested.contains(permission))
        {
            return Err(PermissionGrantError::UndeclaredGrant {
                permission: *permission,
            });
        }
        Ok(self)
    }

    /// Changes one requested permission decision.
    ///
    /// # Errors
    ///
    /// Returns [`PermissionGrantError::NotRequested`] for undeclared permissions.
    pub fn set_granted(
        &mut self,
        permission: RecipePermission,
        granted: bool,
    ) -> Result<(), PermissionGrantError> {
        if !self.requested.contains(&permission) {
            return Err(PermissionGrantError::NotRequested { permission });
        }
        self.granted.retain(|candidate| *candidate != permission);
        if granted {
            self.granted.push(permission);
            self.granted
                .sort_by_key(|candidate| permission_key(*candidate));
        }
        Ok(())
    }
}

fn permission_key(permission: RecipePermission) -> &'static str {
    match permission {
        RecipePermission::ProjectRead => "project.read",
        RecipePermission::ProjectWrite => "project.write",
        RecipePermission::MediaRead => "media.read",
        RecipePermission::MediaImport => "media.import",
        RecipePermission::ExportCreate => "export.create",
        RecipePermission::FilesystemRead => "filesystem.read",
        RecipePermission::FilesystemWrite => "filesystem.write",
        RecipePermission::NetworkAccess => "network.access",
        RecipePermission::PluginInvoke => "plugin.invoke",
    }
}

/// Invalid permission review state.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PermissionGrantError {
    /// A host grant was not declared by the script.
    #[error("permission was granted without being requested: {permission:?}")]
    UndeclaredGrant {
        /// Undeclared capability.
        permission: RecipePermission,
    },
    /// A permission decision targeted an undeclared capability.
    #[error("permission was not requested: {permission:?}")]
    NotRequested {
        /// Undeclared capability.
        permission: RecipePermission,
    },
}

/// Relative executable paths shipped as one reviewed scripting runtime bundle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeBundleManifest {
    /// Manifest schema version.
    pub api_version: u32,
    /// Human-readable bundle release.
    pub version: String,
    /// Optional Python executable relative to the manifest directory.
    pub python: Option<PathBuf>,
    /// Optional Node executable relative to the manifest directory.
    pub node: Option<PathBuf>,
    /// Optional TypeScript compiler executable relative to the manifest directory.
    pub typescript_compiler: Option<PathBuf>,
}

/// Resolved, validated paths from a runtime bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeBundle {
    /// Bundle release identifier.
    pub version: String,
    /// Python worker executable, when packaged.
    pub python: Option<PathBuf>,
    /// Node worker executable, when packaged.
    pub node: Option<PathBuf>,
    /// TypeScript compiler executable, when packaged.
    pub typescript_compiler: Option<PathBuf>,
}

impl RuntimeBundleManifest {
    /// Resolves relative runtime paths against a bundle directory and verifies them.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeBundleError`] for unsupported versions, unsafe paths,
    /// empty bundles, or missing executables.
    pub fn resolve(self, directory: &Path) -> Result<RuntimeBundle, RuntimeBundleError> {
        if self.api_version != RUNTIME_BUNDLE_API_VERSION {
            return Err(RuntimeBundleError::UnsupportedApiVersion {
                found: self.api_version,
            });
        }
        if self.version.trim().is_empty() {
            return Err(RuntimeBundleError::EmptyVersion);
        }
        if self.python.is_none() && self.node.is_none() && self.typescript_compiler.is_none() {
            return Err(RuntimeBundleError::EmptyBundle);
        }
        Ok(RuntimeBundle {
            version: self.version,
            python: resolve_runtime_path(directory, self.python)?,
            node: resolve_runtime_path(directory, self.node)?,
            typescript_compiler: resolve_runtime_path(directory, self.typescript_compiler)?,
        })
    }
}

impl RuntimeBundle {
    /// Returns the packaged interpreter/compiler pair for a recipe language.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeBundleError::MissingLanguage`] when required runtime
    /// components are absent.
    pub fn for_language(
        &self,
        language: RecipeLanguage,
    ) -> Result<(&Path, Option<&Path>), RuntimeBundleError> {
        match language {
            RecipeLanguage::Python => self
                .python
                .as_deref()
                .map(|python| (python, None))
                .ok_or(RuntimeBundleError::MissingLanguage { language }),
            RecipeLanguage::JavaScript => self
                .node
                .as_deref()
                .map(|node| (node, None))
                .ok_or(RuntimeBundleError::MissingLanguage { language }),
            RecipeLanguage::TypeScript => {
                match (self.node.as_deref(), self.typescript_compiler.as_deref()) {
                    (Some(node), Some(compiler)) => Ok((node, Some(compiler))),
                    _ => Err(RuntimeBundleError::MissingLanguage { language }),
                }
            }
        }
    }
}

fn resolve_runtime_path(
    directory: &Path,
    path: Option<PathBuf>,
) -> Result<Option<PathBuf>, RuntimeBundleError> {
    let Some(path) = path else {
        return Ok(None);
    };
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(RuntimeBundleError::UnsafePath(path));
    }
    let resolved = directory.join(path);
    if !resolved.is_file() {
        return Err(RuntimeBundleError::MissingExecutable(resolved));
    }
    Ok(Some(resolved))
}

/// Runtime-bundle manifest or filesystem failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RuntimeBundleError {
    /// The bundle uses a newer schema.
    #[error("unsupported runtime bundle API version {found}")]
    UnsupportedApiVersion {
        /// Supplied version.
        found: u32,
    },
    /// A release identifier is required.
    #[error("runtime bundle version must not be empty")]
    EmptyVersion,
    /// At least one runtime component is required.
    #[error("runtime bundle contains no executables")]
    EmptyBundle,
    /// A component escaped the bundle directory.
    #[error("runtime bundle path must be relative and contained: {0}")]
    UnsafePath(PathBuf),
    /// A declared component was not found.
    #[error("runtime bundle executable does not exist: {0}")]
    MissingExecutable(PathBuf),
    /// Components needed for a language were not packaged.
    #[error("runtime bundle does not support {language:?}")]
    MissingLanguage {
        /// Requested language.
        language: RecipeLanguage,
    },
}

/// A request envelope suitable for JSON-RPC-like transports.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// Correlation identifier supplied by the client.
    pub request_id: String,
    /// Required protocol version.
    pub api_version: String,
    /// Declared operation.
    pub operation: Operation,
}

/// A named atomic script transaction containing one or more operations.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionRequest {
    /// Correlation identifier supplied by the client.
    pub request_id: String,
    /// Required protocol version.
    pub api_version: String,
    /// Project revision observed before the script planned its changes.
    pub expected_revision: u64,
    /// User-visible undo label.
    pub label: String,
    /// Validate and report commands without committing when true.
    pub dry_run: bool,
    /// Ordered operations committed as one transaction.
    pub operations: Vec<Operation>,
}

impl TransactionRequest {
    /// Returns the number of operations requested by the transaction.
    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.operations.len()
    }
}

/// Validated protocol transaction ready for the command layer.
#[derive(Debug)]
pub struct ProtocolTransaction {
    /// User-visible undo label.
    pub label: String,
    /// Revision required for commit.
    pub expected_revision: u64,
    /// Whether the caller requested validation without commit.
    pub dry_run: bool,
    /// Strongly typed commands in request order.
    pub commands: Vec<Command>,
}

/// One validated operation shown by the interactive transaction debugger.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugStep {
    /// Zero-based operation position.
    pub index: usize,
    /// Stable protocol operation discriminator.
    pub operation_type: String,
    /// Whether the debugger should pause before this operation.
    pub breakpoint: bool,
}

/// Cursor state for a validated, non-mutating script transaction debug session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugSnapshot {
    /// Correlation identifier from the transaction.
    pub request_id: String,
    /// Project revision against which the transaction was validated.
    pub revision: u64,
    /// Next operation index, or the step count when complete.
    pub cursor: usize,
    /// Total validated operation count.
    pub step_count: usize,
    /// Operation at the cursor when the session is paused.
    pub current: Option<DebugStep>,
    /// Whether every operation has been inspected.
    pub complete: bool,
}

/// Interactive cursor over an atomically validated transaction plan.
#[derive(Debug)]
pub struct TransactionDebugger {
    request_id: String,
    revision: u64,
    steps: Vec<DebugStep>,
    cursor: usize,
}

impl TransactionDebugger {
    /// Validates a transaction without mutation and creates a debugger cursor.
    ///
    /// Breakpoint indexes outside the operation range are ignored. The entire
    /// transaction is validated before the first step, preserving atomic command
    /// semantics while allowing a UI to inspect its planned operations.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionError`] for protocol, revision, or command failures.
    pub fn start(
        project: &Project,
        request: TransactionRequest,
        breakpoints: &[usize],
    ) -> Result<Self, ExecutionError> {
        let request_id = request.request_id.clone();
        let mut steps = request
            .operations
            .iter()
            .enumerate()
            .map(|(index, operation)| {
                let value = serde_json::to_value(operation)?;
                let operation_type = value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                Ok(DebugStep {
                    index,
                    operation_type,
                    breakpoint: breakpoints.contains(&index),
                })
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        steps.sort_by_key(|step| step.index);
        let transaction = request.into_transaction()?;
        if project.revision() != transaction.expected_revision {
            return Err(ExecutionError::Command(format!(
                "revision conflict: expected {}, actual {}",
                transaction.expected_revision,
                project.revision()
            )));
        }
        CommandEngine::validate_transaction(project, transaction.commands)
            .map_err(|error| ExecutionError::Command(error.to_string()))?;
        Ok(Self {
            request_id,
            revision: project.revision(),
            steps,
            cursor: 0,
        })
    }

    /// Returns the current debugger state.
    #[must_use]
    pub fn snapshot(&self) -> DebugSnapshot {
        DebugSnapshot {
            request_id: self.request_id.clone(),
            revision: self.revision,
            cursor: self.cursor,
            step_count: self.steps.len(),
            current: self.steps.get(self.cursor).cloned(),
            complete: self.cursor >= self.steps.len(),
        }
    }

    /// Advances by one operation and returns the new state.
    pub fn step(&mut self) -> DebugSnapshot {
        self.cursor = self.cursor.saturating_add(1).min(self.steps.len());
        self.snapshot()
    }

    /// Continues until the next breakpoint or completion.
    pub fn continue_to_breakpoint(&mut self) -> DebugSnapshot {
        if self.cursor < self.steps.len() {
            self.cursor += 1;
        }
        while self.cursor < self.steps.len() && !self.steps[self.cursor].breakpoint {
            self.cursor += 1;
        }
        self.snapshot()
    }
}

mod protocol;
mod runtime;

pub use protocol::{
    Operation, ProtocolError, RationalTimeDto, RecipeExecutionPlan, RecipeRequest, TrackKindDto,
    TransitionAlignmentDto, TransitionKindDto,
};
pub use runtime::{
    ExecutionError, ExecutionLimits, ExecutionResult, ExternalScriptRunner, ProcessError,
    ProcessLimits, ProcessOutput, ScriptWorker, TypeScriptCompiler,
};

#[cfg(test)]
mod tests;
