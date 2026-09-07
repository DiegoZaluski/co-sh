//! The language-server client: one running server process plus everything
//! the protocol demands around it.
//!
//! Wraps a [`Transport`] with the pieces an agent needs to actually operate a
//! language server:
//!
//! * **Handshake** — [`LanguageServer::initialize`] builds honest client
//!   capabilities (only what an agent uses), negotiates the position
//!   encoding, announces `initialized`, then mirrors settings through
//!   `workspace/didChangeConfiguration` because some servers read exactly one
//!   of the two channels (opencode ships both for the same reason).
//! * **Document sync** — disk-based [`LanguageServer::touch_file`] /
//!   [`LanguageServer::close_file`] with an LRU bound: the file on disk is
//!   always the source of truth, versions are internal counters, and evicted
//!   documents get a proper `didClose` (opencode never closed documents and
//!   leaked them over long sessions).
//! * **Server→client traffic** — a dispatcher task answers
//!   `workspace/configuration` from our settings, acknowledges capability
//!   registration, replies `null` to progress creation, and answers unknown
//!   requests with `MethodNotFound` (a request left unanswered wedges
//!   servers). Diagnostics and messages surface as [`Event`]s.
//! * **Death** — an exit watcher turns transport termination into
//!   [`ServerState::Exited`] so owners can restart or degrade gracefully.
use std::{
    collections::HashSet,
    fmt, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    str::FromStr as _,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use lsp_types::{
    ConfigurationItem, ConfigurationParams, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    FileChangeType, FileEvent, InitializeResult, InitializedParams, MessageType,
    PublishDiagnosticsParams, RegistrationParams, ServerCapabilities,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, UnregistrationParams,
    VersionedTextDocumentIdentifier,
    notification::{
        DidChangeConfiguration, DidChangeTextDocument,
        DidChangeWatchedFiles as DidChangeWatchedFilesNotification, DidCloseTextDocument,
        DidOpenTextDocument, Exit, Initialized, LogMessage as LogMessageNotification, Notification,
        PublishDiagnostics as PublishDiagnosticsNotification,
        ShowMessage as ShowMessageNotification,
    },
    request::{
        Initialize, RegisterCapability, Request, Shutdown, UnregisterCapability,
        WorkDoneProgressCreate, WorkspaceConfiguration as ConfigurationRequest,
    },
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use super::{
    error::{ExitReason, LspError},
    jsonrpc::{IncomingMessage, RequestId, RpcError, error_codes},
    text::PositionEncoding,
    transport::Transport,
};

/// Grace period for the `shutdown` request during teardown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the outbound queue to drain before killing the
/// process anyway.
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum simultaneously-open documents per server; least-recently-touched
/// documents are `didClose`d when exceeded.
const OPEN_DOCS_CAPACITY: usize = 64;

/// Observable lifecycle of a language-server connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerState {
    /// Process spawned, `initialize` not answered yet.
    Starting,
    /// Handshake complete; normal operation.
    Running,
    /// The transport terminated on its own (crash, EOF, protocol violation).
    Exited { reason: ExitReason },
    /// Graceful teardown via [`LanguageServer::shutdown`].
    Stopped,
}

/// Things the client pushes to its owner while running.
#[derive(Clone, Debug)]
pub enum Event {
    /// Lifecycle transition (also observable through
    /// [`LanguageServer::subscribe_state`]).
    StateChanged(ServerState),
    /// `textDocument/publishDiagnostics`.
    PublishDiagnostics(PublishDiagnosticsParams),
    /// `window/showMessage` (user-relevant server complaints).
    ShowMessage { kind: MessageType, message: String },
    /// `window/logMessage` (chatty server logging).
    LogMessage { kind: MessageType, message: String },
    /// The server dynamically registered a method (e.g. pull diagnostics).
    CapabilityRegistered(Box<str>),
    /// The server released a dynamically registered method.
    CapabilityUnregistered(Box<str>),
}

/// Everything needed to launch one language server process.
#[derive(Clone, Debug)]
pub struct LanguageServerConfig {
    /// Server name used in logs and manager keys (`gopls`,
    /// `rust-analyzer`, …).
    pub name: String,
    /// Executable path (already resolved against PATH by discovery).
    pub binary: PathBuf,
    /// Command-line arguments.
    pub args: Vec<String>,
    /// Extra environment variables layered over the inherited environment.
    pub env: Vec<(String, String)>,
    /// Project root: becomes `rootUri`/workspace folder and the process cwd.
    pub root_path: PathBuf,
    /// Workspace configuration served to `workspace/configuration` requests
    /// and mirrored through `didChangeConfiguration`.
    pub settings: Value,
    /// `initializationOptions` sent inside the `initialize` request.
    pub initialization_options: Value,
    /// Where lifecycle/message/diagnostic events are delivered.
    pub events: Option<mpsc::UnboundedSender<Event>>,
}

impl LanguageServerConfig {
    /// Config with sensible defaults for the required identity fields.
    pub fn new(name: impl Into<String>, binary: PathBuf, root_path: PathBuf) -> Self {
        Self {
            name: name.into(),
            binary,
            args: Vec::new(),
            env: Vec::new(),
            root_path,
            settings: Value::Null,
            initialization_options: Value::Null,
            events: None,
        }
    }
}

struct OpenDoc {
    version: i32,
    hash: u64,
}

/// A running language server. Cheap to clone (internal `Arc`).
#[derive(Clone)]
pub struct LanguageServer {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    root_path: PathBuf,
    transport: Transport,
    child: Mutex<Option<tokio::process::Child>>,
    settings: Value,
    initialization_options: Value,
    capabilities: RwLock<Option<ServerCapabilities>>,
    position_encoding: RwLock<PositionEncoding>,
    docs: Mutex<lru::LruCache<PathBuf, OpenDoc>>,
    registered_methods: Mutex<HashSet<Box<str>>>,
    state_tx: watch::Sender<ServerState>,
    events: Option<mpsc::UnboundedSender<Event>>,
}

impl LanguageServer {
    /// Spawn the server process and wire up all service tasks.
    ///
    /// The connection starts in [`ServerState::Starting`]; call
    /// [`Self::initialize`] to perform the handshake.
    pub fn start(config: LanguageServerConfig) -> Result<Self, LspError> {
        let mut command = tokio::process::Command::new(&config.binary);
        command
            .args(&config.args)
            .envs(
                config
                    .env
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
            .current_dir(&config.root_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|err| {
            LspError::Io(std::io::Error::other(format!(
                "failed to spawn language server `{}` ({}): {err}",
                config.name,
                config.binary.display()
            )))
        })?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        log::info!(
            "starting language server `{}` ({} {:?}) cwd={:?}",
            config.name,
            config.binary.display(),
            config.args,
            config.root_path
        );

        Ok(Self::assemble(
            config,
            stdout,
            stdin,
            Some(stderr),
            Some(Mutex::new(Some(child))),
        ))
    }

    /// Assemble a client over arbitrary streams instead of a spawned process.
    ///
    /// Public extension point: tests drive it over in-memory pipes; embedders
    /// can serve LSP over any byte stream.
    pub fn from_streams<I, O, E>(
        config: LanguageServerConfig,
        input: I,
        output: O,
        stderr: Option<E>,
    ) -> Self
    where
        I: tokio::io::AsyncRead + Unpin + Send + 'static,
        O: tokio::io::AsyncWrite + Unpin + Send + 'static,
        E: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        Self::assemble(config, input, output, stderr, None)
    }

    fn assemble<I, O, E>(
        mut config: LanguageServerConfig,
        input: I,
        output: O,
        stderr: Option<E>,
        child: Option<Mutex<Option<tokio::process::Child>>>,
    ) -> Self
    where
        I: tokio::io::AsyncRead + Unpin + Send + 'static,
        O: tokio::io::AsyncWrite + Unpin + Send + 'static,
        E: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let mut transport = Transport::start(&config.name, input, output, stderr);
        // The dispatcher task owns the incoming queue outright; see its doc
        // comment for why it must not be reachable through the client's Arc.
        let incoming_rx = transport.take_incoming_rx();
        let (state_tx, _) = watch::channel(ServerState::Starting);
        let docs = lru::LruCache::new(
            std::num::NonZeroUsize::new(OPEN_DOCS_CAPACITY).expect("capacity is nonzero"),
        );

        let events = config.events.take();
        let this = Self {
            inner: Arc::new(Inner {
                name: config.name.clone(),
                root_path: config.root_path.clone(),
                transport,
                child: child.unwrap_or_else(|| Mutex::new(None)),
                settings: config.settings.clone(),
                initialization_options: config.initialization_options.clone(),
                capabilities: RwLock::new(None),
                position_encoding: RwLock::new(PositionEncoding::Utf16),
                docs: Mutex::new(docs),
                registered_methods: Mutex::new(HashSet::new()),
                state_tx,
                events,
            }),
        };

        // Service tasks only hold weak references plus narrow cloned
        // dependencies. Strong references here would outlive the owner and
        // deadlock teardown (task waits for Inner to drop, Inner waits for
        // the task's Arc).
        let weak = Arc::downgrade(&this.inner);
        let exit_rx = this.inner.transport.exited();
        tokio::spawn(server_message_loop(weak.clone(), incoming_rx));
        tokio::spawn(exit_watcher(weak, exit_rx));
        this
    }

    /// Configured server name (manager key / logs).
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Current lifecycle state.
    pub fn state(&self) -> ServerState {
        self.inner.state_tx.borrow().clone()
    }

    /// Watch lifecycle transitions.
    pub fn subscribe_state(&self) -> watch::Receiver<ServerState> {
        self.inner.state_tx.subscribe()
    }

    /// Capabilities reported by the server, once initialized.
    pub fn capabilities(&self) -> Option<ServerCapabilities> {
        self.inner
            .capabilities
            .read()
            .expect("capabilities lock")
            .clone()
    }

    /// Position encoding negotiated at initialize (UTF-16 until proven
    /// otherwise — the protocol default when the server says nothing).
    pub fn position_encoding(&self) -> PositionEncoding {
        *self.inner.position_encoding.read().expect("encoding lock")
    }

    /// Underlying transport (request/notification primitives for the typed
    /// query wrappers built on later phases).
    #[allow(dead_code)]
    pub(crate) fn transport(&self) -> &Transport {
        &self.inner.transport
    }

    /// Issue one raw request and await its JSON response.
    ///
    /// Public query surface for tool layers that build their own typed
    /// wrappers on top of the shared transport.
    pub async fn request_raw(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<serde_json::Value, LspError> {
        self.transport().request(method, params, timeout).await
    }

    /// Methods the server dynamically registered (`textDocument/diagnostic`
    /// pulls and friends). Consumed by the diagnostics engine.
    #[allow(dead_code)]
    pub(crate) fn registered_methods(&self) -> HashSet<Box<str>> {
        self.inner
            .registered_methods
            .lock()
            .expect("registered lock")
            .clone()
    }

    // Handshake

    /// Perform the `initialize` handshake and reach [`ServerState::Running`].
    pub async fn initialize(&self, timeout: Duration) -> Result<(), LspError> {
        if self.state() != ServerState::Starting {
            return Err(LspError::InvalidState("initialize called after startup"));
        }
        let root_uri = uri_from_path(&self.inner.root_path)?;
        let folder_name = self
            .inner
            .root_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.inner.name.clone());

        let params = json!({
            "processId": std::process::id(),
            "rootPath": self.inner.root_path.display().to_string(),
            "rootUri": root_uri.as_str(),
            "clientInfo": { "name": "cosh" },
            "initializationOptions": self.inner.initialization_options,
            "workspaceFolders": [{
                "uri": root_uri.as_str(),
                "name": folder_name,
            }],
            "capabilities": client_capabilities(),
        });

        let response = self
            .inner
            .transport
            .request(Initialize::METHOD, Some(params), timeout)
            .await?;
        let result: InitializeResult = serde_json::from_value(response).map_err(|err| {
            LspError::MalformedMessage(format!("invalid initialize result: {err}"))
        })?;

        // Negotiate position encoding: honor the server's pick when it is one
        // we offered, otherwise the spec-mandated UTF-16 default applies.
        let negotiated = result
            .capabilities
            .position_encoding
            .as_ref()
            .and_then(|kind| match kind.as_str() {
                "utf-8" => Some(PositionEncoding::Utf8),
                "utf-16" => Some(PositionEncoding::Utf16),
                other => {
                    log::warn!(
                        "server `{}` picked unsupported position encoding `{other}`; falling back to utf-16",
                        self.name()
                    );
                    None
                }
            })
            .unwrap_or(PositionEncoding::Utf16);
        *self.inner.position_encoding.write().expect("encoding lock") = negotiated;
        *self.inner.capabilities.write().expect("capabilities lock") = Some(result.capabilities);

        self.inner
            .transport
            .notify(Initialized::METHOD, Some(json!(InitializedParams {})))?;

        // Dual-channel settings: `initializationOptions` traveled inside
        // `initialize`; servers that only read the configuration channel get
        // the same payload again here.
        if !self.inner.settings.is_null() {
            self.notify_configuration()?;
        }

        self.set_state(ServerState::Running);
        Ok(())
    }

    /// Mirror current settings through `workspace/didChangeConfiguration`.
    fn notify_configuration(&self) -> Result<(), LspError> {
        self.inner.transport.notify(
            DidChangeConfiguration::METHOD,
            Some(json!({ "settings": self.inner.settings })),
        )
    }

    // Document synchronization

    /// Bring `path` into the server's view of the world, reading from disk.
    ///
    /// First touch sends a synthetic `didChangeWatchedFiles` created-event
    /// (several servers — tsserver, clangd — index only after seeing it),
    /// followed by `didOpen`. Subsequent touches compare content hashes:
    /// unchanged files are a no-op (clangd does not re-emit diagnostics for
    /// no-op changes), changed files go out as a single full-document
    /// `didChange` with an incremented version.
    pub async fn touch_file(&self, path: &Path) -> Result<TouchOutcome, LspError> {
        let text = fs::read_to_string(path).map_err(LspError::Io)?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();

        let uri = uri_from_path(path)?;
        let language_id = language_id_for_path(path);

        // Held across the wire notifications below: they are all synchronous
        // channel sends, so the lock serializes every toucher of one path —
        // a didChange can never overtake the didOpen it depends on, and two
        // concurrent first touches cannot both send didOpen.
        let mut docs = self.inner.docs.lock().expect("documents lock");

        if let Some(doc) = docs.get_mut(path) {
            if doc.hash == hash {
                return Ok(TouchOutcome::Unchanged);
            }
            doc.version += 1;
            doc.hash = hash;
            let version = doc.version;
            let sent = self.send_did_change(path, version, text);
            if sent.is_err() {
                // The session is dying; forget the document so a later
                // touch can start clean.
                docs.pop(path);
            }
            return sent.map(|_| TouchOutcome::Changed);
        }

        let mut evicted = Vec::new();
        while docs.len() >= OPEN_DOCS_CAPACITY {
            match docs.pop_lru() {
                Some((stale, _doc)) => evicted.push(stale),
                None => break,
            }
        }

        // Claim the slot before sending anything: concurrent touches now
        // observe the pending open instead of issuing duplicate didOpen.
        docs.put(path.to_path_buf(), OpenDoc { version: 0, hash });

        // didClose for evictees goes out before the successor opens; the
        // watched-files nudge precedes the open (opencode's tsserver/clangd
        // fix).
        let opened = (|| -> Result<(), LspError> {
            for stale in &evicted {
                self.close_file_internal(stale)?;
            }
            self.inner.transport.notify(
                DidChangeWatchedFilesNotification::METHOD,
                Some(
                    serde_json::to_value(DidChangeWatchedFilesParams {
                        changes: vec![FileEvent {
                            uri: uri.clone(),
                            typ: FileChangeType::CREATED,
                        }],
                    })
                    .expect("serializable"),
                ),
            )?;
            self.inner.transport.notify(
                DidOpenTextDocument::METHOD,
                Some(
                    serde_json::to_value(DidOpenTextDocumentParams {
                        text_document: TextDocumentItem {
                            uri,
                            language_id,
                            version: 0,
                            text,
                        },
                    })
                    .expect("serializable"),
                ),
            )?;
            Ok(())
        })();

        if opened.is_err() {
            // The session is dying; undo the claim so a later touch can
            // start clean.
            docs.pop(path);
        }
        opened.map(|_| TouchOutcome::Opened)
    }

    fn send_did_change(&self, path: &Path, version: i32, text: String) -> Result<(), LspError> {
        let uri = uri_from_path(path)?;
        self.inner.transport.notify(
            DidChangeTextDocument::METHOD,
            Some(
                serde_json::to_value(DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier::new(uri, version),
                    content_changes: vec![TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text,
                    }],
                })
                .expect("serializable"),
            ),
        )
    }

    /// Explicitly close a document (`didClose`). No-op when not open;
    /// returns whether a notification was sent.
    pub fn close_file(&self, path: &Path) -> Result<bool, LspError> {
        let removed = self
            .inner
            .docs
            .lock()
            .expect("documents lock")
            .pop(path)
            .is_some();
        if removed {
            self.close_file_internal(path)?;
        }
        Ok(removed)
    }

    /// Paths currently open on the server side.
    pub fn open_documents(&self) -> Vec<PathBuf> {
        self.inner
            .docs
            .lock()
            .expect("documents lock")
            .iter()
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Notify the server that the on-disk document was saved
    /// (`textDocument/didSave`).
    ///
    /// Servers key expensive post-save pipelines off this signal —
    /// rust-analyzer reruns flycheck (cargo check) on save and never on
    /// `didChange`, and its own file watcher may be unavailable, so without
    /// this notification compile-error diagnostics never regenerate after
    /// an out-of-band disk write. No-op when the document is not open.
    pub fn save_file(&self, path: &Path) -> Result<(), LspError> {
        use lsp_types::{DidSaveTextDocumentParams, TextDocumentIdentifier};

        let open = self
            .inner
            .docs
            .lock()
            .expect("documents lock")
            .get(path)
            .is_some();
        if !open {
            return Ok(());
        }
        let uri = uri_from_path(path)?;
        self.inner.transport.notify(
            "textDocument/didSave",
            Some(
                serde_json::to_value(DidSaveTextDocumentParams {
                    text_document: TextDocumentIdentifier::new(uri),
                    text: None,
                })
                .expect("serializable"),
            ),
        )
    }

    // ── Pull diagnostics (hybrid model; the push side is the dispatcher) ─

    /// Pull diagnostics for one document via `textDocument/diagnostic`.
    ///
    /// Only meaningful when the server dynamically registered the pull
    /// variant ([`Self::registered_methods`]); servers without it answer
    /// `MethodNotFound`, surfaced verbatim so callers fall back to the push
    /// stream. `Ok(None)` means the server answered "unchanged" or partial:
    /// there is nothing new to ingest.
    ///
    /// This is not an optional refinement: servers that see the client
    /// advertising the `diagnostic` capability (this client does) may go
    /// entirely push-silent — rust-analyzer does exactly that — and answer
    /// `workspace/diagnostic/refresh` nudges instead. The only reliable way
    /// to observe their diagnostics is to pull.
    pub async fn pull_diagnostics(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> Result<Option<PublishDiagnosticsParams>, LspError> {
        use lsp_types::request::DocumentDiagnosticRequest as DiagnosticPull;

        let request_uri = uri_from_path(path)?;
        let params = serde_json::to_value(lsp_types::DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier::new(request_uri.clone()),
            identifier: None,
            previous_result_id: None,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .expect("serializable");

        let response = self
            .inner
            .transport
            .request(DiagnosticPull::METHOD, Some(params), timeout)
            .await?;
        let report: lsp_types::DocumentDiagnosticReportResult = serde_json::from_value(response)
            .map_err(|err| {
                LspError::MalformedMessage(format!("invalid diagnostic report: {err}"))
            })?;

        match report {
            lsp_types::DocumentDiagnosticReportResult::Report(
                lsp_types::DocumentDiagnosticReport::Full(full),
            ) => {
                let full = full.full_document_diagnostic_report;
                Ok(Some(PublishDiagnosticsParams::new(
                    request_uri,
                    full.items,
                    None,
                )))
            }
            // The requested document carries the identity in publish params;
            // a pull result has none, so related-only answers are unusable.
            lsp_types::DocumentDiagnosticReportResult::Report(
                lsp_types::DocumentDiagnosticReport::Unchanged(_),
            )
            | lsp_types::DocumentDiagnosticReportResult::Partial(_) => Ok(None),
        }
    }

    fn close_file_internal(&self, path: &Path) -> Result<(), LspError> {
        let uri = uri_from_path(path)?;
        self.inner.transport.notify(
            DidCloseTextDocument::METHOD,
            Some(
                serde_json::to_value(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier::new(uri),
                })
                .expect("serializable"),
            ),
        )
    }

    // Teardown

    /// Graceful teardown: close every open document, request `shutdown`
    /// (bounded by a 5 s timeout), send `exit`, wait — briefly — for the
    /// writer to flush, then kill the process.
    ///
    /// [`ServerState::Stopped`] is claimed *before* the kill so the exit
    /// watcher's Starting/Running guard suppresses a spurious `Exited` event:
    /// with a real process the EOF from death races this function, and an
    /// owner restarting on Exited would respawn servers we just closed on
    /// purpose (Zed claims the state first for the same reason).
    ///
    /// Teardown (flush attempt + kill + state transition) also runs on the
    /// error path; only the first error is reported.
    pub async fn shutdown(&self) -> Result<(), LspError> {
        let result = self.graceful_handshake().await;

        // A wedged server must not hang shutdown past this budget.
        let _ = tokio::time::timeout(SHUTDOWN_FLUSH_TIMEOUT, self.inner.transport.flush()).await;
        self.set_state(ServerState::Stopped);
        self.kill_child().await;

        result
    }

    async fn graceful_handshake(&self) -> Result<(), LspError> {
        for path in self.open_documents() {
            let _ = self.close_file(&path);
        }

        match self
            .inner
            .transport
            .request(Shutdown::METHOD, None, SHUTDOWN_TIMEOUT)
            .await
        {
            Ok(_) => self.inner.transport.notify(Exit::METHOD, None),
            // Server already unresponsive: skip exit, kill below finishes it.
            Err(LspError::Timeout { .. }) | Err(LspError::StreamClosed) => Ok(()),
            Err(err) => Err(err),
        }
    }

    async fn kill_child(&self) {
        // Take the child out first so no lock guard spans the awaits below.
        let child = self.inner.child.lock().expect("child lock").take();
        if let Some(mut child) = child {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }

    // Internals

    fn set_state(&self, state: ServerState) {
        // `send_replace` (not `send`): with the initial receiver dropped,
        // plain `send` fails while nobody is subscribed and the stored state
        // would silently stay stale.
        self.inner.state_tx.send_replace(state.clone());
        self.emit(Event::StateChanged(state));
    }

    fn emit(&self, event: Event) {
        emit_event(&self.inner, event);
    }
}

impl fmt::Debug for LanguageServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LanguageServer")
            .field("name", &self.name())
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

/// What [`LanguageServer::touch_file`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchOutcome {
    /// Document was not open: `didOpen` sent.
    Opened,
    /// Content differed: full `didChange` sent with a new version.
    Changed,
    /// Content identical since last touch: nothing sent.
    Unchanged,
}

/// Convert a filesystem path into a percent-encoded `file://` URI.
///
/// `lsp-types 0.97`'s `Uri` has no `from_file_path`, so we encode manually:
/// everything outside RFC 3986 unreserved characters (plus `/` and the
/// Windows drive colon) is escaped; `\` becomes `/`.
///
/// Known v1 limitations: non-UTF-8 paths are lossily converted, UNC paths
/// (`\\srv\share`) are not special-cased, and relative paths are resolved
/// against the URI root rather than rejected.
pub fn uri_from_path(path: &Path) -> Result<lsp_types::Uri, LspError> {
    use std::fmt::Write as _;

    let raw = path.as_os_str().to_string_lossy();
    let mut uri = String::with_capacity(raw.len() + 8);
    uri.push_str("file://");
    // Absolute unix paths already carry their leading slash; Windows drives
    // ("C:\…") need the third slash added.
    if !raw.starts_with('/') {
        uri.push('/');
    }
    for byte in raw.as_bytes() {
        match byte {
            b'\\' => uri.push('/'),
            b'/' | b':' => uri.push(*byte as char),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(*byte as char)
            }
            other => {
                let _ = write!(uri, "%{other:02X}");
            }
        }
    }

    lsp_types::Uri::from_str(&uri).map_err(|err| {
        LspError::InvalidFrame(format!("{raw:?} is not convertible to a URI: {err}"))
    })
}

/// LSP `languageId` for a file, reusing the SDK's language registry.
///
/// Falls back to the raw extension; a few VS Code-conventional ids differ
/// from naive lowercasing and get explicit overrides (servers key grammar
/// selection off this string).
fn language_id_for_path(path: &Path) -> String {
    use crate::ast::SupportLang;

    match SupportLang::from_path(path) {
        Some(lang) => match lang {
            SupportLang::Tsx => "typescriptreact".into(),
            SupportLang::EmacsLisp => "emacs-lisp".into(),
            SupportLang::ObjC => "objective-c".into(),
            other => other.to_string().to_lowercase(),
        },
        None => path
            .extension()
            .map(|ext| ext.to_string_lossy().into_owned())
            .unwrap_or_else(|| "plaintext".into()),
    }
}

/// Honest client capabilities for an agent.
///
/// Built as JSON so drift in `lsp-types` struct shapes cannot silently change
/// what we announce; deserialization into `InitializeParams` validates.
fn client_capabilities() -> Value {
    json!({
        "general": {
            "positionEncodings": ["utf-8", "utf-16"],
        },
        "workspace": {
            "applyEdit": false,
            "workspaceEdit": { "documentChanges": false },
            "didChangeConfiguration": { "dynamicRegistration": false },
            "didChangeWatchedFiles": { "dynamicRegistration": false },
            "configuration": true,
            "workspaceFolders": true,
            "symbol": { "dynamicRegistration": false },
        },
        "textDocument": {
            "synchronization": {
                "dynamicRegistration": false,
                "willSave": false,
                "willSaveWaitUntil": false,
                "didSave": false,
            },
            "publishDiagnostics": {
                "relatedInformation": true,
                "versionSupport": true,
                "codeDescriptionSupport": false,
                "dataSupport": false,
            },
            // Pull diagnostics announced now so the diagnostics engine can
            // start pulling later without renegotiating capabilities.
            "diagnostic": {
                "dynamicRegistration": true,
                "relatedDocumentSupport": false,
            },
            "definition": { "linkSupport": false },
            "declaration": { "linkSupport": false },
            "typeDefinition": { "linkSupport": false },
            "implementation": { "linkSupport": false },
            "references": {},
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "rename": { "prepareSupport": true },
        },
        "window": {
            "workDoneProgress": false,
            "showMessage": {},
            "logMessage": {},
        },
    })
}

/// Consume every message the server sends until the queue closes.
///
/// Owns the incoming receiver outright and only upgrades to the client state
/// while dispatching a message: an idle dispatcher never keeps the client's
/// `Arc` alive, so owner-drop tears the session down without waiting on us.
async fn server_message_loop(
    inner: std::sync::Weak<Inner>,
    mut incoming_rx: tokio::sync::mpsc::Receiver<IncomingMessage>,
) {
    while let Some(message) = incoming_rx.recv().await {
        let Some(inner) = inner.upgrade() else { break };
        match message {
            IncomingMessage::Request { id, method, params } => {
                handle_server_request(&inner, id, &method, params);
            }
            IncomingMessage::Notification { method, params } => {
                handle_server_notification(&inner, &method, params);
            }
            IncomingMessage::Response { .. } => {
                // Routed to pending requests by the transport; unreachable.
            }
        }
    }
}

fn handle_server_request(inner: &Inner, id: RequestId, method: &str, params: Option<Value>) {
    let server = inner;
    let response: Result<Value, RpcError> = match method {
        ConfigurationRequest::METHOD => match parse_params::<ConfigurationParams>(&params) {
            Ok(config) => Ok(serde_json::to_value(
                config
                    .items
                    .into_iter()
                    .map(|item: ConfigurationItem| lookup_section(&inner.settings, item.section))
                    .collect::<Vec<_>>(),
            )
            .expect("serializable")),
            Err(message) => Err(RpcError::new(error_codes::INVALID_PARAMS, message)),
        },
        RegisterCapability::METHOD => match parse_params::<RegistrationParams>(&params) {
            Ok(registrations) => {
                let mut registered = inner.registered_methods.lock().expect("registered lock");
                for registration in registrations.registrations {
                    log::info!(
                        "server `{}` registered `{}`",
                        inner.name,
                        registration.method
                    );
                    registered.insert(registration.method.clone().into_boxed_str());
                    emit_event(
                        server,
                        Event::CapabilityRegistered(registration.method.into()),
                    );
                }
                Ok(Value::Null)
            }
            Err(message) => Err(RpcError::new(error_codes::INVALID_PARAMS, message)),
        },
        UnregisterCapability::METHOD => match parse_params::<UnregistrationParams>(&params) {
            Ok(unregistrations) => {
                let mut registered = inner.registered_methods.lock().expect("registered lock");
                for unregistration in unregistrations.unregisterations {
                    registered.remove(unregistration.method.as_str());
                    emit_event(
                        server,
                        Event::CapabilityUnregistered(unregistration.method.into()),
                    );
                }
                Ok(Value::Null)
            }
            Err(message) => Err(RpcError::new(error_codes::INVALID_PARAMS, message)),
        },
        WorkDoneProgressCreate::METHOD => {
            // Acknowledged and ignored: progress tokens buy an agent nothing.
            Ok(Value::Null)
        }
        _ => {
            // Never leave a server request hanging: conformant servers stall
            // waiting for the response, and gopls treats missing
            // registerCapability replies as fatal.
            Err(RpcError::new(
                error_codes::METHOD_NOT_FOUND,
                format!("unrecognized method `{method}`"),
            ))
        }
    };

    if let Err(err) = inner.transport.respond(&id, response.as_ref()) {
        log::warn!("failed to answer server request `{method}`: {err}");
    }
}

fn handle_server_notification(inner: &Inner, method: &str, params: Option<Value>) {
    match method {
        PublishDiagnosticsNotification::METHOD => {
            match parse_params::<PublishDiagnosticsParams>(&params) {
                Ok(diag) => emit_event(inner, Event::PublishDiagnostics(diag)),
                Err(message) => log::warn!("invalid publishDiagnostics params: {message}"),
            }
        }
        ShowMessageNotification::METHOD => {
            match parse_params::<lsp_types::ShowMessageParams>(&params) {
                Ok(message) => {
                    log::warn!("server `{}`: {}", inner.name, message.message);
                    emit_event(
                        inner,
                        Event::ShowMessage {
                            kind: message.typ,
                            message: message.message,
                        },
                    );
                }
                Err(detail) => log::warn!("invalid showMessage params: {detail}"),
            }
        }
        LogMessageNotification::METHOD => {
            match parse_params::<lsp_types::LogMessageParams>(&params) {
                Ok(message) => {
                    emit_event(
                        inner,
                        Event::LogMessage {
                            kind: message.typ,
                            message: message.message,
                        },
                    );
                }
                Err(detail) => log::warn!("invalid logMessage params: {detail}"),
            }
        }
        other => log::trace!("ignoring server notification `{other}`"),
    }
}

/// Deserialize params; failures carry the serde detail back to the caller.
fn parse_params<T: serde::de::DeserializeOwned>(params: &Option<Value>) -> Result<T, String> {
    serde_json::from_value(params.clone().unwrap_or(Value::Null))
        .map_err(|err| format!("invalid params: {err}"))
}

/// Dotted-path lookup into the settings object (`"gopls.buildFlags"`); a
/// missing section yields `null`, matching what VS Code would answer.
fn lookup_section(settings: &Value, section: Option<String>) -> Value {
    let Some(section) = section else {
        return settings.clone();
    };
    let mut cursor = settings;
    for segment in section.split('.') {
        cursor = match cursor.get(segment) {
            Some(found) => found,
            None => return Value::Null,
        };
    }
    cursor.clone()
}

/// Flip to [`ServerState::Exited`] when the transport dies on its own.
///
/// Only transitions out of Starting/Running: a graceful
/// [`LanguageServer::shutdown`] already claimed the final state. Holds only a
/// [`Weak`] to the client so it can never block owner teardown.
async fn exit_watcher(
    inner: std::sync::Weak<Inner>,
    mut exited: watch::Receiver<Option<ExitReason>>,
) {
    loop {
        // Observe the current value BEFORE waiting: this task subscribes
        // after the transport's background tasks already exist, so the
        // session may have terminated in between. `changed()` only reports
        // changes past the subscription point and would never fire for a
        // pre-existing terminal reason.
        if let Some(reason) = exited.borrow_and_update().clone() {
            let Some(inner) = inner.upgrade() else { return };
            if matches!(
                inner.state_tx.borrow().clone(),
                ServerState::Starting | ServerState::Running
            ) {
                log::info!("language server `{}` terminated: {reason}", inner.name);
                let state = ServerState::Exited { reason };
                inner.state_tx.send_replace(state.clone());
                emit_event(&inner, Event::StateChanged(state));
            }
            return;
        }
        if exited.changed().await.is_err() {
            return;
        }
    }
}

/// Push an event to the owner, if one is listening.
fn emit_event(inner: &Inner, event: Event) {
    if let Some(events) = &inner.events {
        let _ = events.send(event);
    }
}
