//! Language-neutral scripting protocol boundary.
//!
//! Protocol DTOs are deliberately separate from domain and command types.
//! Conversion is the validation boundary; serialized input never mutates a
//! project directly.

use prism_commands::{Command, CommandEngine};
use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId};
use prism_project::{Project, TrackKind};
use prism_recipe::{RecipeError, RecipeManifest, RecipePermission};
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

/// Limits enforced by the host before a script transaction reaches the command engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionLimits {
    /// Maximum number of operations in one request.
    pub max_operations: usize,
    /// Maximum serialized request size in bytes.
    pub max_payload_bytes: usize,
}

impl ExecutionLimits {
    /// Returns whether a transaction operation count fits within this limit.
    #[must_use]
    pub const fn allows_operations(self, count: usize) -> bool {
        count <= self.max_operations
    }
}

/// Bounds applied to one trusted external language-worker process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessLimits {
    /// Maximum wall-clock execution time.
    pub timeout: Duration,
    /// Maximum bytes collected from either output stream.
    pub max_output_bytes: usize,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Captured output from a completed language-worker process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    /// Bytes written to standard output.
    pub stdout: Vec<u8>,
    /// Bytes written to standard error.
    pub stderr: Vec<u8>,
}

/// Failures supervising an external language worker.
#[derive(Debug, Error)]
pub enum ProcessError {
    /// The executable could not be started or its pipes could not be read.
    #[error("script process I/O error: {0}")]
    Io(#[from] io::Error),
    /// The process exceeded its wall-clock budget.
    #[error("script process timed out after {0:?}")]
    TimedOut(Duration),
    /// Cancellation was requested while the process was running.
    #[error("script process cancelled")]
    Cancelled,
    /// A stream exceeded the configured output budget.
    #[error("script process output exceeds {limit} bytes on {stream}")]
    OutputTooLarge {
        /// Configured stream limit.
        limit: usize,
        /// Stream which exceeded the limit.
        stream: &'static str,
    },
    /// The process exited unsuccessfully.
    #[error("script process exited with status {status}: {stderr}")]
    Failed {
        /// Platform exit status.
        status: String,
        /// Captured diagnostic text.
        stderr: String,
    },
    /// A requested capability was not present in the host grant.
    #[error("script permission was not granted: {permission:?}")]
    PermissionDenied {
        /// Capability requested by the script manifest.
        permission: RecipePermission,
    },
    /// The compiler exited successfully but did not create its expected file.
    #[error("TypeScript compiler did not create expected output: {0}")]
    MissingCompiledOutput(PathBuf),
}

/// Runs a trusted external language worker with bounded lifetime and output.
///
/// The runner deliberately does not claim OS-level sandboxing. Callers must
/// choose an interpreter, script path, environment, and permissions suitable
/// for their trust policy. The returned stdout is expected to contain a
/// language-neutral Prism transaction for subsequent host validation.
pub struct ExternalScriptRunner {
    executable: PathBuf,
    limits: ProcessLimits,
}

impl ExternalScriptRunner {
    /// Creates a runner for an explicit interpreter executable.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>, limits: ProcessLimits) -> Self {
        Self {
            executable: executable.into(),
            limits,
        }
    }

    /// Runs `script` with optional arguments and captures bounded output.
    ///
    /// # Errors
    ///
    /// Returns a structured supervision error when startup, cancellation,
    /// timeout, output limits, or process exit fails.
    pub fn run(
        &self,
        script: &Path,
        arguments: &[PathBuf],
        cancelled: &AtomicBool,
    ) -> Result<ProcessOutput, ProcessError> {
        self.run_with_env(script, arguments, &[], cancelled)
    }

    /// Runs a script while injecting explicit environment variables.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when the process cannot be started, exceeds
    /// its limits, is cancelled, or exits unsuccessfully.
    pub fn run_with_env(
        &self,
        script: &Path,
        arguments: &[PathBuf],
        environment: &[(&str, String)],
        cancelled: &AtomicBool,
    ) -> Result<ProcessOutput, ProcessError> {
        self.run_authorized(script, arguments, environment, &[], &[], cancelled)
    }

    /// Runs a script only after every requested capability has been granted.
    ///
    /// No child process is spawned when authorization fails.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::PermissionDenied`] before process startup when
    /// a requested capability is absent from `granted`, or another process
    /// supervision error after authorization succeeds.
    pub fn run_authorized(
        &self,
        script: &Path,
        arguments: &[PathBuf],
        environment: &[(&str, String)],
        requested: &[RecipePermission],
        granted: &[RecipePermission],
        cancelled: &AtomicBool,
    ) -> Result<ProcessOutput, ProcessError> {
        let mut command_arguments = Vec::with_capacity(arguments.len() + 1);
        command_arguments.push(script.to_path_buf());
        command_arguments.extend_from_slice(arguments);
        self.run_command_authorized(
            &command_arguments,
            environment,
            requested,
            granted,
            cancelled,
        )
    }

    /// Runs the configured executable with an arbitrary argument vector.
    ///
    /// This is used by compiler adapters whose executable does not take a
    /// script path as its first argument.
    ///
    /// # Errors
    ///
    /// Returns the same bounded process and authorization errors as
    /// [`Self::run_authorized`].
    pub fn run_command_authorized(
        &self,
        arguments: &[PathBuf],
        environment: &[(&str, String)],
        requested: &[RecipePermission],
        granted: &[RecipePermission],
        cancelled: &AtomicBool,
    ) -> Result<ProcessOutput, ProcessError> {
        if let Some(permission) = requested
            .iter()
            .find(|permission| !granted.contains(permission))
        {
            return Err(ProcessError::PermissionDenied {
                permission: *permission,
            });
        }
        let mut command = ProcessCommand::new(&self.executable);
        command
            .args(arguments)
            .envs(environment.iter().map(|(key, value)| (*key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("script stdout pipe unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("script stderr pipe unavailable"))?;
        let stdout_thread = spawn_bounded_reader(stdout, self.limits.max_output_bytes, "stdout");
        let stderr_thread = spawn_bounded_reader(stderr, self.limits.max_output_bytes, "stderr");
        let started = Instant::now();
        loop {
            if cancelled.load(Ordering::Acquire) {
                terminate_child(&mut child);
                let _ = join_reader(stdout_thread);
                let _ = join_reader(stderr_thread);
                return Err(ProcessError::Cancelled);
            }
            if started.elapsed() >= self.limits.timeout {
                terminate_child(&mut child);
                let _ = join_reader(stdout_thread);
                let _ = join_reader(stderr_thread);
                return Err(ProcessError::TimedOut(self.limits.timeout));
            }
            if let Some(status) = child.try_wait()? {
                let stdout = join_reader(stdout_thread)?;
                let stderr = join_reader(stderr_thread)?;
                if !status.success() {
                    return Err(ProcessError::Failed {
                        status: status.to_string(),
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    });
                }
                return Ok(ProcessOutput { stdout, stderr });
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Explicit TypeScript compiler adapter backed by the bounded process runner.
pub struct TypeScriptCompiler {
    runner: ExternalScriptRunner,
}

impl TypeScriptCompiler {
    /// Creates an adapter for a pinned or otherwise explicitly selected `tsc`.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>, limits: ProcessLimits) -> Self {
        Self {
            runner: ExternalScriptRunner::new(executable, limits),
        }
    }

    /// Compiles one TypeScript source file into `output_dir`.
    ///
    /// The caller owns cleanup of the output directory. The compiler is run
    /// with a fixed module/target baseline so the resulting file can be passed
    /// to the Node worker consistently.
    ///
    /// # Errors
    ///
    /// Returns a bounded process error or [`ProcessError::MissingCompiledOutput`]
    /// when the compiler does not produce the expected sibling JavaScript file.
    pub fn compile(
        &self,
        source: &Path,
        output_dir: &Path,
        requested: &[RecipePermission],
        granted: &[RecipePermission],
        cancelled: &AtomicBool,
    ) -> Result<PathBuf, ProcessError> {
        fs::create_dir_all(output_dir)?;
        let output_flag = output_dir.to_path_buf();
        let arguments = [
            PathBuf::from("--target"),
            PathBuf::from("ES2022"),
            PathBuf::from("--module"),
            PathBuf::from("NodeNext"),
            PathBuf::from("--outDir"),
            output_flag,
            source.to_path_buf(),
        ];
        self.runner
            .run_command_authorized(&arguments, &[], requested, granted, cancelled)?;
        let stem = source
            .file_stem()
            .ok_or_else(|| ProcessError::MissingCompiledOutput(output_dir.to_path_buf()))?;
        let compiled = output_dir.join(stem).with_extension("js");
        if !compiled.is_file() {
            return Err(ProcessError::MissingCompiledOutput(compiled));
        }
        Ok(compiled)
    }
}

fn spawn_bounded_reader<R: Read + Send + 'static>(
    mut reader: R,
    limit: usize,
    stream: &'static str,
) -> thread::JoinHandle<Result<Vec<u8>, ProcessError>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                return Ok(output);
            }
            if output.len().saturating_add(count) > limit {
                return Err(ProcessError::OutputTooLarge { limit, stream });
            }
            output.extend_from_slice(&buffer[..count]);
        }
    })
}

fn join_reader(
    thread: thread::JoinHandle<Result<Vec<u8>, ProcessError>>,
) -> Result<Vec<u8>, ProcessError> {
    thread
        .join()
        .map_err(|_| io::Error::other("script output reader panicked"))?
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            max_operations: 10_000,
            max_payload_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Structured result returned by a supervised script execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionResult {
    /// Client correlation identifier.
    pub request_id: String,
    /// Project revision after execution.
    pub revision: u64,
    /// Whether the request was validation-only.
    pub dry_run: bool,
    /// Number of validated or committed command events.
    pub event_count: usize,
}

/// Host-side execution failures.
#[derive(Debug, Error)]
pub enum ExecutionError {
    /// Serialized input exceeded the host payload budget.
    #[error("script payload exceeds {limit} bytes")]
    PayloadTooLarge {
        /// Configured payload limit.
        limit: usize,
    },
    /// The operation count exceeded the host budget.
    #[error("script request exceeds {limit} operations")]
    TooManyOperations {
        /// Configured operation limit.
        limit: usize,
    },
    /// The request was cancelled before mutation.
    #[error("script execution cancelled")]
    Cancelled,
    /// JSON decoding or encoding failed.
    #[error("script protocol JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Protocol validation failed.
    #[error("script protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    /// Command validation or execution failed.
    #[error("script command error: {0}")]
    Command(String),
    /// A command required a capability absent from the host grant.
    #[error("script permission was not granted: {permission:?}")]
    PermissionDenied {
        /// Capability required by the transaction.
        permission: RecipePermission,
    },
}

/// Supervised host boundary shared by language-specific workers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScriptWorker {
    /// Resource limits applied before command execution.
    pub limits: ExecutionLimits,
}

impl ScriptWorker {
    /// Executes one serialized transaction against the authoritative project.
    ///
    /// The cancellation flag is checked before any state mutation. Dry-run
    /// requests use the same validation path but never commit project state.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionError`] when the payload is malformed, exceeds the
    /// configured limits, is cancelled, or fails command validation.
    pub fn execute_json(
        &self,
        project: &mut Project,
        payload: &[u8],
        cancelled: &AtomicBool,
    ) -> Result<ExecutionResult, ExecutionError> {
        if payload.len() > self.limits.max_payload_bytes {
            return Err(ExecutionError::PayloadTooLarge {
                limit: self.limits.max_payload_bytes,
            });
        }
        let request: TransactionRequest = serde_json::from_slice(payload)?;
        self.execute(project, request, cancelled)
    }

    /// Executes one already-decoded transaction under host limits.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionError`] when the request exceeds limits, is
    /// cancelled, fails protocol validation, or fails command validation.
    pub fn execute(
        &self,
        project: &mut Project,
        request: TransactionRequest,
        cancelled: &AtomicBool,
    ) -> Result<ExecutionResult, ExecutionError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(ExecutionError::Cancelled);
        }
        if request.operations.len() > self.limits.max_operations {
            return Err(ExecutionError::TooManyOperations {
                limit: self.limits.max_operations,
            });
        }
        let request_id = request.request_id.clone();
        let transaction = request.into_transaction()?;
        let dry_run = transaction.dry_run;
        let event_count = if dry_run {
            CommandEngine::validate_transaction(project, transaction.commands)
                .map_err(|error| ExecutionError::Command(error.to_string()))?
                .len()
        } else {
            CommandEngine::execute_transaction_at_revision(
                project,
                transaction.expected_revision,
                transaction.label,
                transaction.commands,
            )
            .map_err(|error| ExecutionError::Command(error.to_string()))?
            .events
            .len()
        };
        Ok(ExecutionResult {
            request_id,
            revision: project.revision(),
            dry_run,
            event_count,
        })
    }

    /// Executes a transaction after checking command-derived capabilities.
    ///
    /// The permission check occurs before command validation or mutation.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionError::PermissionDenied`] when the transaction's
    /// required capabilities are absent, otherwise the same errors as
    /// [`Self::execute`].
    pub fn execute_authorized(
        &self,
        project: &mut Project,
        request: TransactionRequest,
        cancelled: &AtomicBool,
        granted: &[RecipePermission],
    ) -> Result<ExecutionResult, ExecutionError> {
        if !request.operations.is_empty() && !granted.contains(&RecipePermission::ProjectRead) {
            return Err(ExecutionError::PermissionDenied {
                permission: RecipePermission::ProjectRead,
            });
        }
        if !request.dry_run && !granted.contains(&RecipePermission::ProjectWrite) {
            return Err(ExecutionError::PermissionDenied {
                permission: RecipePermission::ProjectWrite,
            });
        }
        if request
            .operations
            .iter()
            .any(|operation| matches!(operation, Operation::ImportMedia { .. }))
            && !granted.contains(&RecipePermission::MediaImport)
        {
            return Err(ExecutionError::PermissionDenied {
                permission: RecipePermission::MediaImport,
            });
        }
        self.execute(project, request, cancelled)
    }
}

/// Recipe invocation received by the script worker boundary.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeRequest {
    /// Declarative recipe metadata.
    pub manifest: RecipeManifest,
    /// User-supplied parameter values.
    pub inputs: serde_json::Map<String, serde_json::Value>,
}

/// Validated recipe execution plan. Construction performs no script execution.
#[derive(Debug)]
pub struct RecipeExecutionPlan {
    /// Validated recipe metadata.
    pub manifest: RecipeManifest,
    /// Validated user inputs, preserving JSON values for the worker adapter.
    pub inputs: serde_json::Map<String, serde_json::Value>,
}

/// Initial protocol operations. Every mutation converts to a typed command.
#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Operation {
    /// Register a source-media reference.
    ImportMedia {
        /// Stable asset identifier.
        id: Uuid,
        /// User-authorized source path.
        source_path: String,
    },
    /// Create an empty sequence.
    CreateSequence {
        /// Stable identifier encoded as a UUID.
        id: Uuid,
        /// User-visible name.
        name: String,
    },
    /// Add a named exact-time sequence marker.
    AddMarker {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Stable marker identifier.
        id: Uuid,
        /// Exact sequence position.
        time: RationalTimeDto,
        /// User-visible label.
        label: String,
    },
    /// Delete a sequence marker.
    DeleteMarker {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Marker identifier.
        marker_id: Uuid,
    },
    /// Add a transition between adjacent clips.
    AddTransition {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Stable transition ID.
        id: Uuid,
        /// Outgoing clip.
        outgoing_clip: Uuid,
        /// Incoming clip.
        incoming_clip: Uuid,
        /// Exact duration.
        duration: RationalTimeDto,
        /// Transition type.
        kind: TransitionKindDto,
    },
    /// Delete a transition.
    DeleteTransition {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Transition ID.
        transition_id: Uuid,
    },
    /// Create an empty track.
    CreateTrack {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Stable track identifier.
        id: Uuid,
        /// Track media kind.
        kind: TrackKindDto,
    },
    /// Locks or unlocks a track for future editing commands.
    SetTrackLocked {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Track to update.
        track_id: Uuid,
        /// New lock state.
        locked: bool,
    },
    /// Changes whether a track contributes video layers.
    SetTrackVisible {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Track to update.
        track_id: Uuid,
        /// New visibility state.
        visible: bool,
    },
    /// Changes whether a track contributes audio.
    SetTrackMuted {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Track to update.
        track_id: Uuid,
        /// New mute state.
        muted: bool,
    },
    /// Reorders a track within its sequence.
    ReorderTrack {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Track to move.
        track_id: Uuid,
        /// Zero-based destination index.
        destination_index: usize,
    },
    /// Insert a source range on a track.
    InsertClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Destination track.
        track_id: Uuid,
        /// Stable clip identifier.
        id: Uuid,
        /// Referenced media asset.
        media_id: Uuid,
        /// Sequence start.
        timeline_start: RationalTimeDto,
        /// Source start.
        source_start: RationalTimeDto,
        /// Positive source duration.
        source_duration: RationalTimeDto,
    },
    /// Duplicate an existing clip at a new sequence position.
    DuplicateClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Existing clip identifier.
        clip_id: Uuid,
        /// Stable duplicate identifier.
        new_clip_id: Uuid,
        /// New sequence start.
        timeline_start: RationalTimeDto,
    },
    /// Move an existing clip.
    MoveClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
        /// New sequence start.
        timeline_start: RationalTimeDto,
    },
    /// Slip an existing clip's source window without changing its timeline span.
    SlipClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
        /// Exact source-time delta.
        source_delta: RationalTimeDto,
    },
    /// Roll the boundary between two adjacent clips.
    RollEdit {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip before the boundary.
        left_clip_id: Uuid,
        /// Clip after the boundary.
        right_clip_id: Uuid,
        /// Exact boundary delta.
        delta: RationalTimeDto,
    },
    /// Trim an existing clip.
    TrimClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
        /// New sequence start.
        timeline_start: RationalTimeDto,
        /// New source start.
        source_start: RationalTimeDto,
        /// New source duration.
        source_duration: RationalTimeDto,
    },
    /// Split an existing clip.
    SplitClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Existing left clip identifier.
        clip_id: Uuid,
        /// New right clip identifier.
        right_clip_id: Uuid,
        /// Sequence split position.
        timeline_position: RationalTimeDto,
    },
    /// Delete an existing clip.
    DeleteClip {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
    },
    /// Remove a clip and close the gap on its track.
    RippleDelete {
        /// Owning sequence.
        sequence_id: Uuid,
        /// Owning track.
        track_id: Uuid,
        /// Clip identifier.
        clip_id: Uuid,
    },
}

/// Exact serialized time; floating-point seconds are intentionally unsupported.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RationalTimeDto {
    /// Signed tick count.
    pub value: i64,
    /// Positive ticks per second.
    pub timescale: u32,
}

/// Serialized track kind.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackKindDto {
    /// Visual media.
    Video,
    /// Sampled audio.
    Audio,
}

/// Serialized transition kind.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransitionKindDto {
    /// Linear cross-dissolve.
    Dissolve,
}

/// Protocol validation errors returned consistently to every SDK.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolError {
    /// Caller requested an unsupported API contract.
    #[error("unsupported API version: {requested}")]
    UnsupportedApiVersion {
        /// Version supplied by the caller.
        requested: String,
    },
    /// Serialized rational time used a zero timescale.
    #[error("invalid rational time: timescale must be greater than zero")]
    InvalidTimebase,
    /// Recipe metadata or inputs failed validation.
    #[error("invalid recipe: {0}")]
    InvalidRecipe(RecipeError),
}

impl RecipeRequest {
    /// Validates a recipe manifest and its inputs before worker startup.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidRecipe`] when metadata or parameters
    /// fail the recipe safety contract.
    pub fn into_execution_plan(self) -> Result<RecipeExecutionPlan, ProtocolError> {
        self.manifest
            .validate_inputs(&self.inputs)
            .map_err(ProtocolError::InvalidRecipe)?;
        let mut inputs = self.inputs;
        for (name, schema) in &self.manifest.parameters {
            if !inputs.contains_key(name)
                && let Some(default) = &schema.default
            {
                inputs.insert(name.clone(), default.clone());
            }
        }
        Ok(RecipeExecutionPlan {
            manifest: self.manifest,
            inputs,
        })
    }

    /// Authorizes requested permissions and validates inputs before planning.
    ///
    /// This is the worker-host entry point when a caller has an explicit grant
    /// set. [`Self::into_execution_plan`] remains useful for metadata-only
    /// validation tools that do not activate a recipe.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidRecipe`] when permissions, metadata, or
    /// inputs fail validation.
    pub fn authorize_and_plan(
        self,
        granted: &[RecipePermission],
    ) -> Result<RecipeExecutionPlan, ProtocolError> {
        self.manifest
            .authorize(granted)
            .map_err(ProtocolError::InvalidRecipe)?;
        self.into_execution_plan()
    }
}

impl Request {
    /// Validates the envelope and converts the DTO into a domain command.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnsupportedApiVersion`] when the caller's API
    /// contract is not supported.
    pub fn into_command(self) -> Result<Command, ProtocolError> {
        if self.api_version != API_VERSION {
            return Err(ProtocolError::UnsupportedApiVersion {
                requested: self.api_version,
            });
        }
        self.operation.into_command()
    }
}

impl TransactionRequest {
    /// Validates the envelope and converts every operation to a typed command.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnsupportedApiVersion`] when the caller's API
    /// contract is not supported.
    pub fn into_transaction(self) -> Result<ProtocolTransaction, ProtocolError> {
        if self.api_version != API_VERSION {
            return Err(ProtocolError::UnsupportedApiVersion {
                requested: self.api_version,
            });
        }
        Ok(ProtocolTransaction {
            label: self.label,
            expected_revision: self.expected_revision,
            dry_run: self.dry_run,
            commands: self
                .operations
                .into_iter()
                .map(Operation::into_command)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

impl Operation {
    #[allow(clippy::too_many_lines)]
    fn into_command(self) -> Result<Command, ProtocolError> {
        Ok(match self {
            Self::ImportMedia { id, source_path } => Command::ImportMedia {
                id: MediaAssetId::from_uuid(id),
                source_path,
            },
            Self::CreateSequence { id, name } => Command::CreateSequence {
                id: SequenceId::from_uuid(id),
                name,
            },
            Self::AddMarker {
                sequence_id,
                id,
                time,
                label,
            } => Command::AddMarker {
                sequence_id: SequenceId::from_uuid(sequence_id),
                id: MarkerId::from_uuid(id),
                time: time.try_into()?,
                label,
            },
            Self::DeleteMarker {
                sequence_id,
                marker_id,
            } => Command::DeleteMarker {
                sequence_id: SequenceId::from_uuid(sequence_id),
                marker_id: MarkerId::from_uuid(marker_id),
            },
            Self::AddTransition {
                sequence_id,
                id,
                outgoing_clip,
                incoming_clip,
                duration,
                kind,
            } => Command::AddTransition {
                sequence_id: SequenceId::from_uuid(sequence_id),
                id: prism_core::TransitionId::from_uuid(id),
                outgoing_clip: ClipId::from_uuid(outgoing_clip),
                incoming_clip: ClipId::from_uuid(incoming_clip),
                duration: duration.try_into()?,
                kind: kind.into(),
            },
            Self::DeleteTransition {
                sequence_id,
                transition_id,
            } => Command::DeleteTransition {
                sequence_id: SequenceId::from_uuid(sequence_id),
                transition_id: prism_core::TransitionId::from_uuid(transition_id),
            },
            Self::CreateTrack {
                sequence_id,
                id,
                kind,
            } => Command::CreateTrack {
                sequence_id: SequenceId::from_uuid(sequence_id),
                id: TrackId::from_uuid(id),
                kind: kind.into(),
            },
            Self::SetTrackLocked {
                sequence_id,
                track_id,
                locked,
            } => Command::SetTrackLocked {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                locked,
            },
            Self::SetTrackVisible {
                sequence_id,
                track_id,
                visible,
            } => Command::SetTrackVisible {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                visible,
            },
            Self::SetTrackMuted {
                sequence_id,
                track_id,
                muted,
            } => Command::SetTrackMuted {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                muted,
            },
            Self::ReorderTrack {
                sequence_id,
                track_id,
                destination_index,
            } => Command::ReorderTrack {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                destination_index,
            },
            Self::InsertClip {
                sequence_id,
                track_id,
                id,
                media_id,
                timeline_start,
                source_start,
                source_duration,
            } => Command::InsertClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                id: ClipId::from_uuid(id),
                media_id: MediaAssetId::from_uuid(media_id),
                timeline_start: timeline_start.try_into()?,
                source_start: source_start.try_into()?,
                source_duration: source_duration.try_into()?,
            },
            Self::DuplicateClip {
                sequence_id,
                track_id,
                clip_id,
                new_clip_id,
                timeline_start,
            } => Command::DuplicateClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                new_clip_id: ClipId::from_uuid(new_clip_id),
                timeline_start: timeline_start.try_into()?,
            },
            Self::MoveClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
            } => Command::MoveClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                timeline_start: timeline_start.try_into()?,
            },
            Self::SlipClip {
                sequence_id,
                track_id,
                clip_id,
                source_delta,
            } => Command::SlipClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                source_delta: source_delta.try_into()?,
            },
            Self::RollEdit {
                sequence_id,
                track_id,
                left_clip_id,
                right_clip_id,
                delta,
            } => Command::RollEdit {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                left_clip_id: ClipId::from_uuid(left_clip_id),
                right_clip_id: ClipId::from_uuid(right_clip_id),
                delta: delta.try_into()?,
            },
            Self::TrimClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
                source_start,
                source_duration,
            } => Command::TrimClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                timeline_start: timeline_start.try_into()?,
                source_start: source_start.try_into()?,
                source_duration: source_duration.try_into()?,
            },
            Self::SplitClip {
                sequence_id,
                track_id,
                clip_id,
                right_clip_id,
                timeline_position,
            } => Command::SplitClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
                right_clip_id: ClipId::from_uuid(right_clip_id),
                timeline_position: timeline_position.try_into()?,
            },
            Self::DeleteClip {
                sequence_id,
                track_id,
                clip_id,
            } => Command::DeleteClip {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
            },
            Self::RippleDelete {
                sequence_id,
                track_id,
                clip_id,
            } => Command::RippleDelete {
                sequence_id: SequenceId::from_uuid(sequence_id),
                track_id: TrackId::from_uuid(track_id),
                clip_id: ClipId::from_uuid(clip_id),
            },
        })
    }
}

impl TryFrom<RationalTimeDto> for RationalTime {
    type Error = ProtocolError;

    fn try_from(value: RationalTimeDto) -> Result<Self, Self::Error> {
        RationalTime::new(value.value, value.timescale).map_err(|_| ProtocolError::InvalidTimebase)
    }
}

impl From<TrackKindDto> for TrackKind {
    fn from(value: TrackKindDto) -> Self {
        match value {
            TrackKindDto::Video => Self::Video,
            TrackKindDto::Audio => Self::Audio,
        }
    }
}

impl From<TransitionKindDto> for prism_project::TransitionKind {
    fn from(value: TransitionKindDto) -> Self {
        match value {
            TransitionKindDto::Dissolve => Self::Dissolve,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        API_VERSION, ExecutionError, ExecutionLimits, ExternalScriptRunner, ProcessError,
        ProcessLimits, ProtocolError, RecipePermission, RecipeRequest, Request, ScriptWorker,
        TransactionRequest, TypeScriptCompiler,
    };
    use prism_project::Project;
    use std::sync::atomic::AtomicBool;
    use uuid::Uuid;

    #[test]
    fn rejects_unknown_api_version_before_command_execution() -> Result<(), serde_json::Error> {
        let json = r#"{
            "requestId":"req-1",
            "apiVersion":"999",
            "operation":{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}
        }"#;
        let request: Request = serde_json::from_str(json)?;
        assert!(matches!(
            request.into_command(),
            Err(ProtocolError::UnsupportedApiVersion { requested }) if requested == "999"
        ));
        Ok(())
    }

    #[test]
    fn accepts_current_api_version() -> Result<(), serde_json::Error> {
        let json = format!(
            r#"{{"requestId":"req-1","apiVersion":"{API_VERSION}","operation":{{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}}}}"#
        );
        let request: Request = serde_json::from_str(&json)?;
        assert!(request.into_command().is_ok());
        Ok(())
    }

    #[test]
    fn converts_atomic_batch_and_preserves_dry_run() -> Result<(), Box<dyn std::error::Error>> {
        let json = format!(
            r#"{{
                "requestId":"req-2",
                "apiVersion":"{API_VERSION}",
                "expectedRevision":7,
                "label":"Prepare timeline",
                "dryRun":true,
                "operations":[
                    {{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}},
                    {{"type":"createSequence","id":"00000000-0000-0000-0000-000000000002","name":"Social"}}
                ]
            }}"#
        );
        let request: TransactionRequest = serde_json::from_str(&json)?;
        let transaction = request.into_transaction()?;
        assert!(transaction.dry_run);
        assert_eq!(transaction.expected_revision, 7);
        assert_eq!(transaction.label, "Prepare timeline");
        assert_eq!(transaction.commands.len(), 2);
        Ok(())
    }

    #[test]
    fn converts_track_reorder_operation() -> Result<(), Box<dyn std::error::Error>> {
        let request: Request = serde_json::from_value(serde_json::json!({
            "requestId": "reorder",
            "apiVersion": API_VERSION,
            "operation": {
                "type": "reorderTrack",
                "sequenceId": "00000000-0000-0000-0000-000000000001",
                "trackId": "00000000-0000-0000-0000-000000000002",
                "destinationIndex": 1
            }
        }))?;
        assert!(request.into_command().is_ok());
        Ok(())
    }

    #[test]
    fn converts_track_visibility_and_mute_operations() -> Result<(), Box<dyn std::error::Error>> {
        for operation in [
            serde_json::json!({"type": "setTrackVisible", "sequenceId": "00000000-0000-0000-0000-000000000001", "trackId": "00000000-0000-0000-0000-000000000002", "visible": false}),
            serde_json::json!({"type": "setTrackMuted", "sequenceId": "00000000-0000-0000-0000-000000000001", "trackId": "00000000-0000-0000-0000-000000000002", "muted": false}),
        ] {
            let request: Request = serde_json::from_value(serde_json::json!({
                "requestId": "track-state",
                "apiVersion": API_VERSION,
                "operation": operation
            }))?;
            assert!(request.into_command().is_ok());
        }
        Ok(())
    }

    #[test]
    fn converts_marker_operations() -> Result<(), Box<dyn std::error::Error>> {
        for operation in [
            serde_json::json!({"type": "addMarker", "sequenceId": "00000000-0000-0000-0000-000000000001", "id": "00000000-0000-0000-0000-000000000002", "time": {"value": 3, "timescale": 2}, "label": "Beat"}),
            serde_json::json!({"type": "deleteMarker", "sequenceId": "00000000-0000-0000-0000-000000000001", "markerId": "00000000-0000-0000-0000-000000000002"}),
        ] {
            let request: Request = serde_json::from_value(serde_json::json!({
                "requestId": "marker",
                "apiVersion": API_VERSION,
                "operation": operation
            }))?;
            assert!(request.into_command().is_ok());
        }
        Ok(())
    }

    #[test]
    fn converts_duplicate_clip_operation() -> Result<(), Box<dyn std::error::Error>> {
        let request: Request = serde_json::from_value(serde_json::json!({
            "requestId": "duplicate",
            "apiVersion": API_VERSION,
            "operation": {
                "type": "duplicateClip",
                "sequenceId": "00000000-0000-0000-0000-000000000001",
                "trackId": "00000000-0000-0000-0000-000000000002",
                "clipId": "00000000-0000-0000-0000-000000000003",
                "newClipId": "00000000-0000-0000-0000-000000000004",
                "timelineStart": {"value": 4, "timescale": 1}
            }
        }))?;
        assert!(request.into_command().is_ok());
        Ok(())
    }

    #[test]
    fn converts_ripple_delete_operation() -> Result<(), Box<dyn std::error::Error>> {
        let request: Request = serde_json::from_value(serde_json::json!({
            "requestId": "ripple",
            "apiVersion": API_VERSION,
            "operation": {
                "type": "rippleDelete",
                "sequenceId": "00000000-0000-0000-0000-000000000001",
                "trackId": "00000000-0000-0000-0000-000000000002",
                "clipId": "00000000-0000-0000-0000-000000000003"
            }
        }))?;
        assert!(request.into_command().is_ok());
        Ok(())
    }

    #[test]
    fn converts_transition_operations() -> Result<(), Box<dyn std::error::Error>> {
        for operation in [
            serde_json::json!({"type":"addTransition","sequenceId":"00000000-0000-0000-0000-000000000001","id":"00000000-0000-0000-0000-000000000002","outgoingClip":"00000000-0000-0000-0000-000000000003","incomingClip":"00000000-0000-0000-0000-000000000004","duration":{"value":1,"timescale":2},"kind":"dissolve"}),
            serde_json::json!({"type":"deleteTransition","sequenceId":"00000000-0000-0000-0000-000000000001","transitionId":"00000000-0000-0000-0000-000000000002"}),
        ] {
            let request: Request = serde_json::from_value(
                serde_json::json!({"requestId":"transition","apiVersion":API_VERSION,"operation":operation}),
            )?;
            assert!(request.into_command().is_ok());
        }
        Ok(())
    }

    #[test]
    fn worker_honors_limits_cancellation_and_dry_run() -> Result<(), Box<dyn std::error::Error>> {
        let request = serde_json::json!({
            "requestId": "worker-test", "apiVersion": API_VERSION,
            "expectedRevision": 0, "label": "Create sequence", "dryRun": true,
            "operations": [{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}]
        });
        let payload = serde_json::to_vec(&request)?;
        let worker = ScriptWorker {
            limits: ExecutionLimits {
                max_operations: 1,
                max_payload_bytes: payload.len(),
            },
        };
        let mut project = Project::new("Worker");
        let cancelled = AtomicBool::new(false);
        let result = worker.execute_json(&mut project, &payload, &cancelled)?;
        assert!(result.dry_run);
        assert_eq!(result.event_count, 1);
        assert_eq!(project.revision(), 0);
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            worker.execute_json(&mut project, &payload, &cancelled),
            Err(ExecutionError::Cancelled)
        ));
        let limited = ScriptWorker {
            limits: ExecutionLimits {
                max_operations: 0,
                max_payload_bytes: payload.len(),
            },
        };
        cancelled.store(false, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            limited.execute_json(&mut project, &payload, &cancelled),
            Err(ExecutionError::TooManyOperations { limit: 0 })
        ));
        Ok(())
    }

    #[test]
    fn rejects_zero_timescale_at_protocol_boundary() -> Result<(), Box<dyn std::error::Error>> {
        let json = format!(
            r#"{{
                "requestId":"req-3",
                "apiVersion":"{API_VERSION}",
                "operation":{{
                    "type":"moveClip",
                    "sequenceId":"00000000-0000-0000-0000-000000000001",
                    "trackId":"00000000-0000-0000-0000-000000000002",
                    "clipId":"00000000-0000-0000-0000-000000000003",
                    "timelineStart":{{"value":1,"timescale":0}}
                }}
            }}"#
        );
        let request: Request = serde_json::from_str(&json)?;
        assert!(matches!(
            request.into_command(),
            Err(ProtocolError::InvalidTimebase)
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn external_runner_captures_output_and_enforces_cancellation()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::path::PathBuf;
        use std::sync::atomic::Ordering;
        let runner = ExternalScriptRunner::new(
            "/bin/sh",
            ProcessLimits {
                timeout: std::time::Duration::from_secs(2),
                max_output_bytes: 128,
            },
        );
        let cancelled = AtomicBool::new(false);
        let output = runner.run(
            PathBuf::from("-c").as_path(),
            &[PathBuf::from("printf prism")],
            &cancelled,
        )?;
        assert_eq!(output.stdout, b"prism");
        assert!(output.stderr.is_empty());

        cancelled.store(true, Ordering::Release);
        assert!(matches!(
            runner.run(
                PathBuf::from("-c").as_path(),
                &[PathBuf::from("printf never")],
                &cancelled,
            ),
            Err(ProcessError::Cancelled)
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn external_runner_rejects_unbounded_output() {
        let runner = ExternalScriptRunner::new(
            "/bin/sh",
            ProcessLimits {
                timeout: std::time::Duration::from_secs(2),
                max_output_bytes: 4,
            },
        );
        let cancelled = AtomicBool::new(false);
        assert!(matches!(
            runner.run(
                std::path::Path::new("-c"),
                &[std::path::PathBuf::from("printf 12345")],
                &cancelled,
            ),
            Err(ProcessError::OutputTooLarge {
                limit: 4,
                stream: "stdout"
            })
        ));
    }

    #[test]
    fn external_runner_denies_capability_before_spawn() {
        let runner =
            ExternalScriptRunner::new("/path/that/does/not/exist", ProcessLimits::default());
        let cancelled = AtomicBool::new(false);
        assert!(matches!(
            runner.run_authorized(
                std::path::Path::new("script.py"),
                &[],
                &[],
                &[RecipePermission::NetworkAccess],
                &[],
                &cancelled,
            ),
            Err(ProcessError::PermissionDenied {
                permission: RecipePermission::NetworkAccess
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn typescript_compiler_uses_explicit_output_directory() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("prism-ts-{}", Uuid::new_v4()));
        std::fs::create_dir(&root)?;
        let compiler_path = root.join("tsc");
        let source = root.join("main.ts");
        let output = root.join("out");
        std::fs::write(
            &compiler_path,
            "#!/bin/sh\nout=''\nsrc=''\nprevious=''\nfor arg in \"$@\"; do if [ \"$previous\" = \"--outDir\" ]; then out=\"$arg\"; fi; src=\"$arg\"; previous=\"$arg\"; done\nbase=$(basename \"$src\" .ts)\nprintf '%s' 'console.log(\"compiled\")' > \"$out/$base.js\"\n",
        )?;
        std::fs::set_permissions(&compiler_path, std::fs::Permissions::from_mode(0o755))?;
        std::fs::write(&source, "const value: number = 1;\n")?;
        let compiled_path = TypeScriptCompiler::new(&compiler_path, ProcessLimits::default())
            .compile(&source, &output, &[], &[], &AtomicBool::new(false))?;
        assert_eq!(compiled_path, output.join("main.js"));
        assert!(compiled_path.is_file());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn checked_in_transaction_example_matches_protocol() -> Result<(), Box<dyn std::error::Error>> {
        let request: TransactionRequest = serde_json::from_str(include_str!(
            "../../../examples/script-transaction-dry-run.json"
        ))?;
        let transaction = request.into_transaction()?;
        assert_eq!(transaction.expected_revision, 1);
        assert!(transaction.dry_run);
        assert_eq!(transaction.commands.len(), 1);
        Ok(())
    }

    #[test]
    fn recipe_request_becomes_a_validated_execution_plan() -> Result<(), Box<dyn std::error::Error>>
    {
        let request: RecipeRequest = serde_json::from_value(serde_json::json!({
            "manifest": {
                "id": "prism.example.recipe",
                "version": "1.0.0",
                "apiVersion": 1,
                "language": "typescript",
                "entry": "main.ts",
                "permissions": ["project.read"],
                "parameters": {
                    "clipCount": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 20,
                        "default": 5
                    }
                }
            },
            "inputs": {"clipCount": 3}
        }))?;
        let plan = request.into_execution_plan()?;
        assert_eq!(plan.manifest.id, "prism.example.recipe");
        assert_eq!(plan.inputs["clipCount"], serde_json::json!(3));
        let authorized_request = RecipeRequest {
            manifest: plan.manifest.clone(),
            inputs: plan.inputs.clone(),
        };
        assert!(matches!(
            authorized_request.authorize_and_plan(&[]),
            Err(ProtocolError::InvalidRecipe(_))
        ));
        let authorized_request = RecipeRequest {
            manifest: plan.manifest,
            inputs: plan.inputs,
        };
        assert!(
            authorized_request
                .authorize_and_plan(&[RecipePermission::ProjectRead])
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn recipe_execution_plan_materializes_declared_defaults()
    -> Result<(), Box<dyn std::error::Error>> {
        let request: RecipeRequest = serde_json::from_value(serde_json::json!({
            "manifest": {
                "id": "prism.example.defaults",
                "version": "1.0.0",
                "apiVersion": 1,
                "language": "python",
                "entry": "main.py",
                "permissions": [],
                "parameters": {
                    "clipCount": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 5
                    },
                    "label": {
                        "type": "string",
                        "default": "Review"
                    }
                }
            },
            "inputs": {}
        }))?;
        let plan = request.into_execution_plan()?;
        assert_eq!(plan.inputs["clipCount"], serde_json::json!(5));
        assert_eq!(plan.inputs["label"], serde_json::json!("Review"));
        Ok(())
    }

    #[test]
    fn invalid_recipe_inputs_are_rejected_before_planning() -> Result<(), Box<dyn std::error::Error>>
    {
        let request: RecipeRequest = serde_json::from_value(serde_json::json!({
            "manifest": {
                "id": "prism.example.recipe",
                "version": "1.0.0",
                "apiVersion": 1,
                "language": "python",
                "entry": "main.py",
                "permissions": [],
                "parameters": {}
            },
            "inputs": {"unexpected": true}
        }))?;
        assert!(matches!(
            request.into_execution_plan(),
            Err(ProtocolError::InvalidRecipe(_))
        ));
        Ok(())
    }
}
