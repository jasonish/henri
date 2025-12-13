// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! LSP (Language Server Protocol) client implementation.
//!
//! This module provides a client that can communicate with LSP servers like rust-analyzer
//! to provide real-time diagnostics for files being edited.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use lsp_types::{
    ClientCapabilities, Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializedParams, TextDocumentContentChangeEvent,
    TextDocumentItem, Uri, VersionedTextDocumentIdentifier, WorkspaceFolder,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::error::{Error, Result};

// LSP request timeout in seconds
const REQUEST_TIMEOUT_SECS: u64 = 30;

// Default wait time for diagnostics in milliseconds
const DIAGNOSTIC_WAIT_MS: u64 = 500;

// Maximum diagnostics to display
const MAX_ERRORS_DISPLAYED: usize = 10;
const MAX_WARNINGS_DISPLAYED: usize = 5;

/// Convert a file path to an LSP Uri
fn path_to_uri(path: &Path) -> Option<Uri> {
    let url = url::Url::from_file_path(path).ok()?;
    Uri::from_str(url.as_str()).ok()
}

/// Convert an LSP Uri to a file path
fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let url = url::Url::parse(uri.as_str()).ok()?;
    url.to_file_path().ok()
}

/// Configuration for an LSP server
#[derive(Debug, Clone)]
pub struct LspServerConfig {
    /// User-friendly name for the server
    pub name: String,
    /// Command to run (e.g., "rust-analyzer")
    pub command: String,
    /// Arguments to pass to the command
    pub args: Vec<String>,
    /// File extensions this server handles (e.g., ["rs"])
    pub file_extensions: Vec<String>,
    /// Root path for the project
    pub root_path: PathBuf,
}

/// A diagnostic with file context
#[derive(Debug, Clone)]
pub struct FileDiagnostic {
    pub file_path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub message: String,
    pub severity: DiagnosticSeverity,
}

impl FileDiagnostic {
    /// Format the diagnostic for display
    pub(crate) fn format(&self) -> String {
        let severity = match self.severity {
            DiagnosticSeverity::ERROR => "error",
            DiagnosticSeverity::WARNING => "warning",
            DiagnosticSeverity::INFORMATION => "info",
            DiagnosticSeverity::HINT => "hint",
            _ => "diagnostic",
        };
        format!(
            "{}:{}:{}: {}: {}",
            self.file_path.display(),
            self.line + 1, // Convert to 1-indexed for display
            self.column + 1,
            severity,
            self.message
        )
    }
}

/// JSON-RPC request
#[derive(Debug, Serialize)]
struct Request {
    jsonrpc: &'static str,
    id: i64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

/// JSON-RPC notification (no id, no response expected)
#[derive(Debug, Serialize)]
struct Notification {
    jsonrpc: &'static str,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

/// JSON-RPC response
#[derive(Debug, Deserialize)]
struct Response {
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default, rename = "error")]
    _error: Option<ResponseError>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ResponseError {
    #[serde(rename = "code")]
    _code: i64,
    #[serde(rename = "message")]
    _message: String,
}

/// A running LSP server client
struct LspClient {
    name: String,
    _process: Child,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    next_id: AtomicI64,
    pending_requests: Arc<RwLock<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>>,
    diagnostics: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>,
    file_extensions: Vec<String>,
    _root_path: PathBuf,
    _reader_handle: JoinHandle<()>,
    opened_files: Arc<RwLock<HashMap<PathBuf, i32>>>,
}

impl LspClient {
    /// Spawn a new LSP server and initialize it
    async fn spawn(config: &LspServerConfig) -> Result<Self> {
        let mut cmd = Command::new(&config.command);
        for arg in &config.args {
            cmd.arg(arg);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());

        let mut process = cmd.spawn().map_err(|e| {
            Error::Lsp(format!(
                "Failed to spawn LSP server '{}': {}",
                config.command, e
            ))
        })?;

        let stdin = process.stdin.take().ok_or_else(|| {
            Error::Lsp(format!(
                "Failed to get stdin for LSP server '{}'",
                config.command
            ))
        })?;

        let stdout = process.stdout.take().ok_or_else(|| {
            Error::Lsp(format!(
                "Failed to get stdout for LSP server '{}'",
                config.command
            ))
        })?;

        let stdin = Arc::new(Mutex::new(stdin));
        let pending_requests: Arc<RwLock<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let diagnostics: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Spawn reader task
        let pending_clone = pending_requests.clone();
        let diagnostics_clone = diagnostics.clone();
        let reader_handle = tokio::spawn(async move {
            Self::reader_loop(stdout, pending_clone, diagnostics_clone).await;
        });

        let mut client = Self {
            name: config.name.clone(),
            _process: process,
            stdin,
            next_id: AtomicI64::new(1),
            pending_requests,
            diagnostics,
            file_extensions: config.file_extensions.clone(),
            _root_path: config.root_path.clone(),
            _reader_handle: reader_handle,
            opened_files: Arc::new(RwLock::new(HashMap::new())),
        };

        // Initialize the server
        client.initialize(&config.root_path).await?;

        Ok(client)
    }

    /// Reader loop that processes messages from the LSP server
    async fn reader_loop(
        stdout: tokio::process::ChildStdout,
        pending_requests: Arc<RwLock<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>>,
        diagnostics: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader as TokioBufReader};

        let mut reader = TokioBufReader::new(stdout);

        loop {
            // Read headers until we get a blank line
            let mut content_length: usize = 0;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => return, // EOF
                    Ok(_) => {
                        let line = line.trim();
                        if line.is_empty() {
                            break;
                        }
                        if let Some(len) = line.strip_prefix("Content-Length: ") {
                            content_length = len.parse().unwrap_or(0);
                        }
                    }
                    Err(_) => return,
                }
            }

            if content_length == 0 {
                continue;
            }

            // Read the content
            let mut content = vec![0u8; content_length];
            if reader.read_exact(&mut content).await.is_err() {
                return;
            }

            // Parse the JSON
            let response: Response = match serde_json::from_slice(&content) {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Check if it's a notification (has method but no id)
            if let Some(method) = &response.method {
                if method == "textDocument/publishDiagnostics"
                    && let Some(params) = response.params
                    && let Ok(diag_params) =
                        serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params)
                    && let Some(path) = uri_to_path(&diag_params.uri)
                {
                    let mut diags = diagnostics.write().await;
                    diags.insert(path, diag_params.diagnostics);
                }
                continue;
            }

            // It's a response to a request
            if let Some(id) = response.id {
                let mut pending = pending_requests.write().await;
                if let Some(sender) = pending.remove(&id) {
                    let result = response.result.unwrap_or(Value::Null);
                    let _ = sender.send(result);
                }
            }
        }
    }

    /// Send a request and wait for response
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = self.pending_requests.write().await;
            pending.insert(id, tx);
        }

        let request = Request {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        self.send_message(&request).await?;

        // Wait for response with timeout
        match tokio::time::timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS), rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(Error::Lsp("Request channel closed".to_string())),
            Err(_) => Err(Error::Lsp(format!("Request '{}' timed out", method))),
        }
    }

    /// Send a notification (no response expected)
    async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = Notification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        };
        self.send_message(&notification).await
    }

    /// Send a message to the LSP server
    async fn send_message<T: Serialize>(&self, message: &T) -> Result<()> {
        let content = serde_json::to_string(message)
            .map_err(|e| Error::Lsp(format!("Failed to serialize message: {}", e)))?;

        let header = format!("Content-Length: {}\r\n\r\n", content.len());

        let mut stdin = self.stdin.lock().await;
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(header.as_bytes())
            .await
            .map_err(|e| Error::Lsp(format!("Failed to write header: {}", e)))?;
        stdin
            .write_all(content.as_bytes())
            .await
            .map_err(|e| Error::Lsp(format!("Failed to write content: {}", e)))?;
        stdin
            .flush()
            .await
            .map_err(|e| Error::Lsp(format!("Failed to flush: {}", e)))?;

        Ok(())
    }

    /// Initialize the LSP server
    #[allow(deprecated)] // root_path and root_uri are deprecated but still used for compatibility
    async fn initialize(&mut self, root_path: &Path) -> Result<()> {
        let root_uri = path_to_uri(root_path).ok_or_else(|| {
            Error::Lsp(format!(
                "Failed to convert path to URI: {}",
                root_path.display()
            ))
        })?;

        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_path: Some(root_path.to_string_lossy().to_string()),
            root_uri: Some(root_uri.clone()),
            capabilities: ClientCapabilities {
                text_document: Some(lsp_types::TextDocumentClientCapabilities {
                    publish_diagnostics: Some(
                        lsp_types::PublishDiagnosticsClientCapabilities::default(),
                    ),
                    synchronization: Some(lsp_types::TextDocumentSyncClientCapabilities {
                        dynamic_registration: Some(false),
                        will_save: Some(false),
                        will_save_wait_until: Some(false),
                        did_save: Some(true),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: root_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "workspace".to_string()),
            }]),
            ..Default::default()
        };

        let _result = self
            .request("initialize", Some(serde_json::to_value(params).unwrap()))
            .await?;

        // Send initialized notification
        self.notify(
            "initialized",
            Some(serde_json::to_value(InitializedParams {}).unwrap()),
        )
        .await?;

        Ok(())
    }

    /// Notify the server that a file was opened
    async fn did_open(&self, path: &Path, content: &str) -> Result<()> {
        let uri = path_to_uri(path)
            .ok_or_else(|| Error::Lsp(format!("Invalid path: {}", path.display())))?;

        let language_id = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| match ext {
                "rs" => "rust",
                "py" => "python",
                "js" => "javascript",
                "ts" => "typescript",
                "go" => "go",
                _ => ext,
            })
            .unwrap_or("plaintext");

        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri,
                language_id: language_id.to_string(),
                version: 1,
                text: content.to_string(),
            },
        };

        // Track this file as opened
        {
            let mut opened = self.opened_files.write().await;
            opened.insert(path.to_path_buf(), 1);
        }

        self.notify(
            "textDocument/didOpen",
            Some(serde_json::to_value(params).unwrap()),
        )
        .await
    }

    /// Notify the server that a file was changed
    async fn did_change(&self, path: &Path, content: &str) -> Result<()> {
        let uri = path_to_uri(path)
            .ok_or_else(|| Error::Lsp(format!("Invalid path: {}", path.display())))?;

        // Get and increment version
        let version = {
            let mut opened = self.opened_files.write().await;
            let version = opened.entry(path.to_path_buf()).or_insert(0);
            *version += 1;
            *version
        };

        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier { uri, version },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None, // Full document sync
                range_length: None,
                text: content.to_string(),
            }],
        };

        self.notify(
            "textDocument/didChange",
            Some(serde_json::to_value(params).unwrap()),
        )
        .await
    }

    /// Get diagnostics for a file
    async fn get_diagnostics(&self, path: &Path) -> Vec<FileDiagnostic> {
        let diags = self.diagnostics.read().await;
        diags
            .get(path)
            .map(|d| {
                d.iter()
                    .map(|diag| FileDiagnostic {
                        file_path: path.to_path_buf(),
                        line: diag.range.start.line,
                        column: diag.range.start.character,
                        message: diag.message.clone(),
                        severity: diag.severity.unwrap_or(DiagnosticSeverity::ERROR),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Check if this client handles files with the given extension
    fn handles_extension(&self, ext: &str) -> bool {
        self.file_extensions.iter().any(|e| e == ext)
    }
}

/// Manager for multiple LSP server connections
pub struct LspManager {
    clients: RwLock<Vec<LspClient>>,
}

impl LspManager {
    pub(crate) fn new() -> Self {
        Self {
            clients: RwLock::new(Vec::new()),
        }
    }

    /// Start an LSP server
    pub async fn start_server(&self, config: &LspServerConfig) -> Result<()> {
        let mut clients = self.clients.write().await;
        if clients.iter().any(|c| c.name == config.name) {
            return Ok(());
        }
        let client = LspClient::spawn(config).await?;
        clients.push(client);
        Ok(())
    }

    /// Notify that a file was opened or changed
    /// This will automatically open the file with the LSP if not already open
    pub async fn notify_file_changed(&self, path: &Path, content: &str) -> Result<()> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        let clients = self.clients.read().await;
        for client in clients.iter() {
            if client.handles_extension(ext) {
                // Check if file is already opened
                let is_opened = {
                    let opened = client.opened_files.read().await;
                    opened.contains_key(&path)
                };

                if is_opened {
                    client.did_change(&path, content).await?;
                } else {
                    client.did_open(&path, content).await?;
                }
            }
        }

        Ok(())
    }

    /// Get diagnostics for a file with default wait time
    pub async fn get_diagnostics_with_wait(&self, path: &Path) -> Vec<FileDiagnostic> {
        self.get_diagnostics_with_custom_wait(path, DIAGNOSTIC_WAIT_MS)
            .await
    }

    /// Get diagnostics for a file with custom wait time
    async fn get_diagnostics_with_custom_wait(
        &self,
        path: &Path,
        wait_ms: u64,
    ) -> Vec<FileDiagnostic> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        // Wait for LSP to process
        tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;

        let clients = self.clients.read().await;
        for client in clients.iter() {
            if client.handles_extension(ext) {
                return client.get_diagnostics(&path).await;
            }
        }

        Vec::new()
    }

    /// Check if any LSP server handles the given file extension
    pub async fn handles_file(&self, path: &Path) -> bool {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let clients = self.clients.read().await;
        clients.iter().any(|c| c.handles_extension(ext))
    }

    /// Get the number of connected LSP servers
    pub async fn server_count(&self) -> usize {
        let clients = self.clients.read().await;
        clients.len()
    }

    /// Shutdown all LSP servers
    pub async fn shutdown_all(&self) {
        let mut clients = self.clients.write().await;
        clients.clear();
        // Dropping the clients will kill the processes
    }
}

/// Global LSP manager instance
static LSP_MANAGER: std::sync::OnceLock<Arc<LspManager>> = std::sync::OnceLock::new();

/// Get the global LSP manager
pub(crate) fn manager() -> Arc<LspManager> {
    LSP_MANAGER
        .get_or_init(|| Arc::new(LspManager::new()))
        .clone()
}

/// Initialize LSP servers from configuration
pub async fn initialize(servers: Vec<LspServerConfig>) -> Result<()> {
    let mgr = manager();
    for config in servers {
        if let Err(_e) = mgr.start_server(&config).await {
            // LSP initialization is optional - silently ignore failures
        }
    }
    Ok(())
}

/// Reload LSP servers from configuration file
pub async fn reload_from_config(working_dir: &std::path::Path) -> Result<usize> {
    use crate::config::ConfigFile;

    let mgr = manager();

    // Shutdown existing servers
    mgr.shutdown_all().await;

    // Load config and restart servers
    let config_file = ConfigFile::load().unwrap_or_default();

    if !config_file.lsp_enabled {
        return Ok(0);
    }

    let Some(lsp_config) = &config_file.lsp else {
        return Ok(0);
    };

    let servers: Vec<LspServerConfig> = lsp_config
        .servers
        .iter()
        .filter(|s| s.enabled)
        .map(|s| LspServerConfig {
            name: s.name.clone(),
            command: s.command.clone(),
            args: s.args.clone(),
            file_extensions: s.file_extensions.clone(),
            root_path: working_dir.to_path_buf(),
        })
        .collect();

    let count = servers.len();
    initialize(servers).await?;
    Ok(count)
}

/// Format diagnostics for inclusion in tool results
pub(crate) fn format_diagnostics(diagnostics: &[FileDiagnostic]) -> String {
    if diagnostics.is_empty() {
        return String::new();
    }

    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::ERROR)
        .collect();
    let warnings: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::WARNING)
        .collect();

    let mut result = String::new();
    result.push_str("\n\n--- LSP Diagnostics ---\n");

    if !errors.is_empty() {
        result.push_str(&format!("Errors ({}):\n", errors.len()));
        for diag in errors.iter().take(MAX_ERRORS_DISPLAYED) {
            result.push_str(&format!("  {}\n", diag.format()));
        }
        if errors.len() > MAX_ERRORS_DISPLAYED {
            result.push_str(&format!(
                "  ... and {} more errors\n",
                errors.len() - MAX_ERRORS_DISPLAYED
            ));
        }
    }

    if !warnings.is_empty() {
        result.push_str(&format!("Warnings ({}):\n", warnings.len()));
        for diag in warnings.iter().take(MAX_WARNINGS_DISPLAYED) {
            result.push_str(&format!("  {}\n", diag.format()));
        }
        if warnings.len() > MAX_WARNINGS_DISPLAYED {
            result.push_str(&format!(
                "  ... and {} more warnings\n",
                warnings.len() - MAX_WARNINGS_DISPLAYED
            ));
        }
    }

    result
}

/// Returns a short summary like "LSP diagnostics: 2 errors, 1 warning" if there are any.
pub(crate) fn diagnostic_summary(diagnostics: &[FileDiagnostic]) -> Option<String> {
    let errors = diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::ERROR)
        .count();
    let warnings = diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::WARNING)
        .count();

    if errors == 0 && warnings == 0 {
        return None;
    }

    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!(
            "{} error{}",
            errors,
            if errors == 1 { "" } else { "s" }
        ));
    }
    if warnings > 0 {
        parts.push(format!(
            "{} warning{}",
            warnings,
            if warnings == 1 { "" } else { "s" }
        ));
    }
    Some(format!("LSP diagnostics: {}", parts.join(", ")))
}
