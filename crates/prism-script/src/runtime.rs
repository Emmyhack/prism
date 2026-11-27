use super::{
    AtomicBool, Child, CommandEngine, Duration, Error, Instant, Operation, Ordering, Path, PathBuf,
    ProcessCommand, Project, ProtocolError, Read, RecipePermission, Serialize, Stdio,
    TransactionRequest, fs, io, thread,
};

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
        self.compile_internal(source, output_dir, requested, granted, cancelled, false)
    }

    /// Compiles TypeScript and requires the compiler to emit a sibling source map.
    ///
    /// # Errors
    ///
    /// Returns a bounded process error or [`ProcessError::MissingCompiledOutput`]
    /// when JavaScript or source-map output is missing.
    pub fn compile_with_source_map(
        &self,
        source: &Path,
        output_dir: &Path,
        requested: &[RecipePermission],
        granted: &[RecipePermission],
        cancelled: &AtomicBool,
    ) -> Result<(PathBuf, PathBuf), ProcessError> {
        let compiled =
            self.compile_internal(source, output_dir, requested, granted, cancelled, true)?;
        let source_map = compiled.with_extension("js.map");
        if !source_map.is_file() {
            return Err(ProcessError::MissingCompiledOutput(source_map));
        }
        Ok((compiled, source_map))
    }

    fn compile_internal(
        &self,
        source: &Path,
        output_dir: &Path,
        requested: &[RecipePermission],
        granted: &[RecipePermission],
        cancelled: &AtomicBool,
        source_map: bool,
    ) -> Result<PathBuf, ProcessError> {
        fs::create_dir_all(output_dir)?;
        let output_flag = output_dir.to_path_buf();
        let mut arguments = vec![
            PathBuf::from("--target"),
            PathBuf::from("ES2022"),
            PathBuf::from("--module"),
            PathBuf::from("NodeNext"),
            PathBuf::from("--outDir"),
            output_flag,
        ];
        if source_map {
            arguments.push(PathBuf::from("--sourceMap"));
        }
        arguments.push(source.to_path_buf());
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
            if project.revision() != transaction.expected_revision {
                return Err(ExecutionError::Command(format!(
                    "revision conflict: expected {}, actual {}",
                    transaction.expected_revision,
                    project.revision()
                )));
            }
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
