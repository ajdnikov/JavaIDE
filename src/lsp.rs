//! Client for the Eclipse JDT Language Server, the Java engine behind VS Code's
//! Java extension. It supplies real completion (with automatic imports) and
//! live diagnostics. All protocol I/O runs on background threads.

use crate::complete::{Item, Kind};
use crate::java::Jdk;
use crate::proc::no_window;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The language server itself needs at least this Java version.
pub const MIN_JAVA: u32 = 21;

/// Zero-based line and UTF-16 column, as used by the protocol.
pub type Pos = (usize, usize);

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Starting(String),
    Ready,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub start: Pos,
    pub end: Pos,
    /// 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u8,
    pub message: String,
    /// The server's own record, sent back when asking for quick fixes.
    pub raw: Arc<Value>,
}

/// An entry of the Ctrl+. menu.
#[derive(Clone, Debug)]
pub struct CodeAction {
    pub title: String,
    pub kind: String,
    pub raw: Arc<Value>,
}

/// One step of a workspace edit.
#[derive(Clone, Debug)]
pub enum FileOp {
    Edit { path: PathBuf, version: Option<i64>, edits: Vec<TextEdit> },
    Create { path: PathBuf, overwrite: bool, ignore_if_exists: bool },
    Rename { from: PathBuf, to: PathBuf, overwrite: bool, ignore_if_exists: bool },
    Delete { path: PathBuf, ignore_if_missing: bool },
}

/// What running a code action needs from the editor.
pub enum ActionStep {
    Apply { ops: Vec<FileOp>, then: Option<Value> },
    Waiting,
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct TextEdit {
    pub start: Pos,
    pub end: Pos,
    pub text: String,
}

/// Server-side data carried by a completion item.
#[derive(Clone, Debug)]
pub struct ItemData {
    pub filter: String,
    pub sort: String,
    /// Where the item's text edit starts.
    pub start: Option<Pos>,
    pub raw: Arc<Value>,
}

pub enum Event {
    Diagnostics { path: PathBuf, items: Vec<Diag> },
    Completion { id: u64, items: Vec<Item> },
    Resolved { id: u64, edits: Vec<TextEdit> },
    CodeActions { id: u64, items: Vec<CodeAction> },
    ActionResolved { action: Value },
    /// The server wants this edit applied; answer with `reply_apply_edit`.
    ApplyEdit { request: Value, ops: Vec<FileOp> },
    CommandFailed { message: String },
}

enum Pending {
    Initialize,
    Completion,
    Resolve,
    CodeActions,
    ActionResolve,
    Command,
}

struct Shared {
    status: Mutex<Status>,
    pending: Mutex<HashMap<u64, Pending>>,
    /// Commands the server can execute itself.
    commands: Mutex<Vec<String>>,
    initialized: AtomicBool,
    stopping: AtomicBool,
    settings: Value,
    folders: Value,
}

struct Doc {
    version: i64,
    text: String,
}

/// Ties the server's lifetime to the IDE: if the IDE dies, Windows ends the server too.
#[cfg(windows)]
mod job {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    const EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time: i64,
        per_job_user_time: i64,
        limit_flags: u32,
        min_working_set: usize,
        max_working_set: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io_counters: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(job: *mut c_void, class: i32, info: *const c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    pub struct Job(*mut c_void);

    // A job handle may be used and closed from any thread.
    unsafe impl Send for Job {}

    impl Job {
        pub fn for_child(child: &Child) -> Option<Job> {
            // SAFETY: the pointers are valid for each call and the handles are owned here.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let job = Job(handle);
                let mut limits = ExtendedLimits::default();
                limits.basic.limit_flags = KILL_ON_JOB_CLOSE;
                let size = std::mem::size_of::<ExtendedLimits>() as u32;
                let info = (&raw const limits).cast::<c_void>();
                if SetInformationJobObject(job.0, EXTENDED_LIMIT_INFORMATION, info, size) == 0 {
                    return None;
                }
                (AssignProcessToJobObject(job.0, child.as_raw_handle().cast()) != 0).then_some(job)
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateJobObjectW and is closed only here.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod job {
    pub struct Job;

    impl Job {
        pub fn for_child(_child: &std::process::Child) -> Option<Job> {
            None
        }
    }
}

pub struct Lsp {
    pub root: PathBuf,
    /// Serving the folder of a lone file rather than an opened project.
    pub single_file: bool,
    job: Option<job::Job>,
    child: Option<Child>,
    tx: Sender<Vec<u8>>,
    events: Receiver<Event>,
    shared: Arc<Shared>,
    next_id: u64,
    docs: HashMap<String, Doc>,
}

// ------------------------------------------------------------ locating the server

fn launcher_jar(server: &Path) -> Option<PathBuf> {
    std::fs::read_dir(server.join("plugins")).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with("org.eclipse.equinox.launcher_") && n.ends_with(".jar")
        })
    })
}

/// The bundled `jdtls` folder next to the exe (or `JAVAIDE_JDTLS` for development).
pub fn find_server() -> Option<PathBuf> {
    let exe = crate::settings::exe_dir();
    let mut candidates: Vec<PathBuf> = std::env::var_os("JAVAIDE_JDTLS").map(PathBuf::from).into_iter().collect();
    candidates.push(exe.join("jdtls"));
    if let Some(p) = exe.parent() {
        candidates.push(p.join("jdtls"));
    }
    candidates.into_iter().find(|c| launcher_jar(c).is_some())
}

fn config_dir_name() -> &'static str {
    if cfg!(windows) {
        "config_win"
    } else if cfg!(target_os = "macos") {
        "config_mac"
    } else {
        "config_linux"
    }
}

/// Server state lives next to the exe so the IDE stays portable.
fn data_root() -> PathBuf {
    let portable = crate::settings::exe_dir().join("jdtls-data");
    if std::fs::create_dir_all(&portable).is_ok() {
        portable
    } else {
        std::env::temp_dir().join("javaide-jdtls-data")
    }
}

fn stable_hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn server_command(jdk: &Jdk, server: &Path, root: &Path) -> Result<Command, String> {
    let jar = launcher_jar(server).ok_or("The language server launcher is missing.")?;
    // One state folder per project, so several IDE windows can run side by side.
    let base = data_root().join(format!("ws-{:016x}", stable_hash(&path_key(root))));
    let data = base.join("data");
    let config = base.join("config");
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&config).map_err(|e| e.to_string())?;

    let mut c = Command::new(jdk.tool("java"));
    c.current_dir(root)
        .args([
            "-XX:+UseParallelGC",
            "-XX:GCTimeRatio=4",
            "-XX:AdaptiveSizePolicyWeight=90",
            "-Dsun.zip.disableMemoryMapping=true",
            "-Xmx2G",
            "-Xms100m",
            "-Xlog:disable",
            "-Djdk.xml.maxGeneralEntitySizeLimit=0",
            "-Djdk.xml.totalEntitySizeLimit=0",
            "-Declipse.application=org.eclipse.jdt.ls.core.id1",
            "-Dosgi.bundles.defaultStartLevel=4",
            "-Declipse.product=org.eclipse.jdt.ls.core.product",
            "-Dosgi.checkConfiguration=true",
        ])
        .arg(format!("-Dosgi.sharedConfiguration.area={}", server.join(config_dir_name()).display()))
        .args([
            "-Dosgi.sharedConfiguration.area.readOnly=true",
            "-Dosgi.configuration.cascaded=true",
            "-Dfile.encoding=UTF-8",
            "-Dstdout.encoding=UTF-8",
            "-Djava.import.generatesMetadataFilesAtProjectRoot=false",
            "-Daether.dependencyCollector.impl=bf",
            "--add-modules=ALL-SYSTEM",
            "--add-opens",
            "java.base/java.util=ALL-UNNAMED",
            "--add-opens",
            "java.base/java.lang=ALL-UNNAMED",
            "--add-opens",
            "java.base/sun.nio.fs=ALL-UNNAMED",
            "-jar",
        ])
        .arg(jar)
        .arg("-configuration")
        .arg(config)
        .arg("-data")
        .arg(data);
    Ok(c)
}

// ------------------------------------------------------------ settings

/// The defaults of VS Code's Java extension, adapted to this editor.
fn java_settings(root: &Path, jdk: &Jdk, single_file: bool) -> Value {
    let home = jdk.home.as_ref().map(|h| h.to_string_lossy().to_string());
    let runtimes = match &home {
        Some(h) => json!([{ "name": format!("JavaSE-{}", jdk.major), "path": h, "default": true }]),
        None => json!([]),
    };
    let src = crate::java::source_root(root);
    let rel = src.strip_prefix(root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
    let source_paths: Vec<String> = if rel.is_empty() { Vec::new() } else { vec![rel] };
    json!({
        "home": home,
        "jdt": { "ls": { "java": { "home": home } } },
        "configuration": { "updateBuildConfiguration": "automatic", "runtimes": runtimes },
        "autobuild": { "enabled": true },
        "import": {
            // A lone file's folder may be anything (Downloads...): don't import builds from it.
            "maven": { "enabled": !single_file },
            "gradle": { "enabled": !single_file },
            "exclusions": ["**/node_modules/**", "**/.metadata/**", "**/archetype-resources/**", "**/META-INF/maven/**"]
        },
        "project": { "referencedLibraries": ["lib/**/*.jar"], "sourcePaths": source_paths },
        "errors": { "incompleteClasspath": { "severity": "warning" } },
        "completion": {
            "enabled": true,
            "engine": "ecj",
            "postfix": { "enabled": true },
            "chain": { "enabled": false },
            "favoriteStaticMembers": [
                "org.junit.Assert.*", "org.junit.Assume.*",
                "org.junit.jupiter.api.Assertions.*", "org.junit.jupiter.api.Assumptions.*",
                "org.junit.jupiter.api.DynamicContainer.*", "org.junit.jupiter.api.DynamicTest.*",
                "org.mockito.Mockito.*", "org.mockito.ArgumentMatchers.*", "org.mockito.Answers.*"
            ],
            "filteredTypes": ["java.awt.*", "com.sun.*", "sun.*", "jdk.*", "org.graalvm.*", "io.micrometer.shaded.*"],
            "guessMethodArguments": "auto",
            "matchCase": "firstLetter",
            "importOrder": ["#", "java", "javax", "org", "com", ""],
            "lazyResolveTextEdit": { "enabled": false },
            "maxResults": 0,
            "collapseCompletionItems": false
        }
    })
}

fn init_params(root: &Path, settings: &Value, folders: &Value) -> Value {
    let uri = path_to_uri(root);
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "JavaIDE" },
        "rootUri": uri,
        "rootPath": root.to_string_lossy(),
        "workspaceFolders": folders,
        "capabilities": {
            "workspace": {
                "applyEdit": true,
                "workspaceEdit": {
                    "documentChanges": true,
                    "resourceOperations": ["create", "rename", "delete"],
                    "failureHandling": "abort"
                },
                "executeCommand": { "dynamicRegistration": false },
                "configuration": true,
                "workspaceFolders": true,
                "didChangeConfiguration": { "dynamicRegistration": false },
                "didChangeWatchedFiles": { "dynamicRegistration": false }
            },
            "textDocument": {
                "synchronization": { "didSave": true, "dynamicRegistration": false },
                "completion": {
                    "contextSupport": true,
                    "completionItem": {
                        "snippetSupport": true,
                        "labelDetailsSupport": true,
                        "documentationFormat": ["plaintext"],
                        "resolveSupport": { "properties": ["additionalTextEdits", "documentation", "detail"] }
                    },
                    "completionItemKind": { "valueSet": (1..=25).collect::<Vec<u8>>() }
                },
                "codeAction": {
                    "codeActionLiteralSupport": {
                        "codeActionKind": {
                            "valueSet": ["", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports"]
                        }
                    },
                    "isPreferredSupport": true,
                    "disabledSupport": true,
                    "dataSupport": true,
                    "resolveSupport": { "properties": ["edit"] }
                },
                "publishDiagnostics": { "relatedInformation": false }
            }
        },
        "initializationOptions": {
            "workspaceFolders": [uri],
            "settings": { "java": settings },
            "extendedClientCapabilities": { "progressReportProvider": true, "classFileContentsSupport": false }
        }
    })
}

// ------------------------------------------------------------ the client

fn frame(v: &Value) -> Vec<u8> {
    let body = v.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

fn notification(method: &str, params: Value) -> Vec<u8> {
    frame(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

impl Lsp {
    /// `single_file`: `root` is merely the folder of a file opened on its own.
    pub fn start(
        server: &Path,
        jdk: &Jdk,
        root: &Path,
        single_file: bool,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Lsp, String> {
        let mut cmd = server_command(jdk, server, root)?;
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("Could not start the language server: {e}"))?;
        let job = job::Job::for_child(&child);
        let mut stdin = child.stdin.take().ok_or("No stdin for the language server")?;
        let stdout = child.stdout.take().ok_or("No stdout for the language server")?;

        let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "workspace".into());
        let folders = json!([{ "uri": path_to_uri(root), "name": name }]);
        let settings = java_settings(root, jdk, single_file);
        let params = init_params(root, &settings, &folders);
        let shared = Arc::new(Shared {
            status: Mutex::new(Status::Starting("Starting".into())),
            pending: Mutex::new(HashMap::from([(1, Pending::Initialize)])),
            commands: Mutex::new(Vec::new()),
            initialized: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            settings,
            folders,
        });

        let (tx, out_rx) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Ok(msg) = out_rx.recv() {
                if stdin.write_all(&msg).and_then(|()| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        let (ev_tx, events) = channel();
        {
            let (shared, tx) = (shared.clone(), tx.clone());
            std::thread::spawn(move || reader_loop(stdout, tx, ev_tx, shared, wake));
        }
        let _ = tx.send(frame(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": params })));
        Ok(Lsp { root: root.to_path_buf(), single_file, job, child: Some(child), tx, events, shared, next_id: 2, docs: HashMap::new() })
    }

    pub fn status(&self) -> Status {
        self.shared.status.lock().map(|s| s.clone()).unwrap_or(Status::Failed("Language server error".into()))
    }

    pub fn is_ready(&self) -> bool {
        self.status() == Status::Ready
    }

    pub fn poll(&mut self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    fn request(&mut self, method: &str, params: Value, kind: Pending) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        if let Ok(mut p) = self.shared.pending.lock() {
            p.insert(id, kind);
        }
        let _ = self.tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })));
        id
    }

    /// Bring the server's view of the open documents up to date.
    pub fn sync<'a>(&mut self, open: impl Iterator<Item = (&'a Path, &'a str)>) {
        if !self.shared.initialized.load(Ordering::SeqCst) {
            return;
        }
        let mut seen = Vec::new();
        for (path, text) in open {
            let uri = path_to_uri(path);
            match self.docs.get_mut(&uri) {
                Some(d) if d.text == text => {}
                Some(d) => {
                    d.version += 1;
                    d.text = text.to_string();
                    let _ = self.tx.send(notification(
                        "textDocument/didChange",
                        json!({
                            "textDocument": { "uri": uri, "version": d.version },
                            "contentChanges": [{ "text": text }]
                        }),
                    ));
                }
                None => {
                    let _ = self.tx.send(notification(
                        "textDocument/didOpen",
                        json!({ "textDocument": { "uri": uri, "languageId": "java", "version": 1, "text": text } }),
                    ));
                    self.docs.insert(uri.clone(), Doc { version: 1, text: text.to_string() });
                }
            }
            seen.push(uri);
        }
        let closed: Vec<String> = self.docs.keys().filter(|u| !seen.contains(u)).cloned().collect();
        for uri in closed {
            self.docs.remove(&uri);
            let _ = self.tx.send(notification("textDocument/didClose", json!({ "textDocument": { "uri": uri } })));
        }
    }

    pub fn did_save(&mut self, path: &Path) {
        let uri = path_to_uri(path);
        if self.docs.contains_key(&uri) {
            let _ = self.tx.send(notification("textDocument/didSave", json!({ "textDocument": { "uri": uri } })));
        }
    }

    /// Report files changed on disk: 1 created, 2 changed, 3 deleted.
    pub fn files_changed(&mut self, changes: &[(PathBuf, u8)]) {
        if !self.shared.initialized.load(Ordering::SeqCst) {
            return;
        }
        let list: Vec<Value> = changes.iter().map(|(p, t)| json!({ "uri": path_to_uri(p), "type": t })).collect();
        let _ = self.tx.send(notification("workspace/didChangeWatchedFiles", json!({ "changes": list })));
    }

    pub fn request_completion(&mut self, path: &Path, pos: Pos, trigger: Option<char>) -> Option<u64> {
        let uri = path_to_uri(path);
        if !self.is_ready() || !self.docs.contains_key(&uri) {
            return None;
        }
        let context = match trigger {
            Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string() }),
            None => json!({ "triggerKind": 1 }),
        };
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": pos.0, "character": pos.1 },
            "context": context
        });
        Some(self.request("textDocument/completion", params, Pending::Completion))
    }

    pub fn cancel(&mut self, id: u64) {
        if let Ok(mut p) = self.shared.pending.lock() {
            p.remove(&id);
        }
        let _ = self.tx.send(notification("$/cancelRequest", json!({ "id": id })));
    }

    pub fn resolve(&mut self, raw: &Value) -> u64 {
        self.request("completionItem/resolve", raw.clone(), Pending::Resolve)
    }

    pub fn doc_version(&self, path: &Path) -> Option<i64> {
        self.docs.get(&path_to_uri(path)).map(|d| d.version)
    }

    /// Ask for the quick fixes and refactorings available at `range`.
    pub fn request_code_actions(&mut self, path: &Path, range: (Pos, Pos), diagnostics: Vec<Value>) -> Option<u64> {
        let uri = path_to_uri(path);
        if !self.is_ready() || !self.docs.contains_key(&uri) {
            return None;
        }
        let params = json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": range.0.0, "character": range.0.1 },
                "end": { "line": range.1.0, "character": range.1.1 }
            },
            "context": { "diagnostics": diagnostics, "triggerKind": 1 }
        });
        Some(self.request("textDocument/codeAction", params, Pending::CodeActions))
    }

    /// Start a chosen code action. `resolved` means the server already filled it in.
    pub fn run_action(&mut self, action: &Value, resolved: bool) -> ActionStep {
        if let Some(edit) = action.get("edit") {
            let then = action.get("command").filter(|c| c.is_object()).cloned();
            return ActionStep::Apply { ops: parse_workspace_edit(edit), then };
        }
        if !resolved && action.get("data").is_some() {
            self.request("codeAction/resolve", action.clone(), Pending::ActionResolve);
            return ActionStep::Waiting;
        }
        match action.get("command") {
            Some(Value::String(_)) => self.run_command(action),
            Some(c @ Value::Object(_)) => {
                let c = c.clone();
                self.run_command(&c)
            }
            _ => ActionStep::Unsupported,
        }
    }

    pub fn run_command(&mut self, command: &Value) -> ActionStep {
        let name = command["command"].as_str().unwrap_or("");
        let args = command.get("arguments").cloned().unwrap_or_else(|| json!([]));
        if name == APPLY_EDIT_COMMAND {
            return ActionStep::Apply { ops: parse_workspace_edit(&args[0]), then: None };
        }
        let params = json!({ "command": name, "arguments": args });
        self.request("workspace/executeCommand", params, Pending::Command);
        ActionStep::Waiting
    }

    pub fn reply_apply_edit(&mut self, request: &Value, applied: bool) {
        let _ = self.tx.send(frame(&json!({ "jsonrpc": "2.0", "id": request, "result": { "applied": applied } })));
    }

    /// Ask the server to exit; with `wait`, block until it has (or kill it).
    pub fn stop(&mut self, wait: bool) {
        let Some(mut child) = self.child.take() else { return };
        self.shared.stopping.store(true, Ordering::SeqCst);
        if self.shared.initialized.load(Ordering::SeqCst) {
            let id = self.next_id;
            self.next_id += 1;
            let _ = self.tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "method": "shutdown" })));
            let _ = self.tx.send(frame(&json!({ "jsonrpc": "2.0", "method": "exit" })));
        }
        let job = self.job.take();
        let mut finish = move || {
            let _keep_alive_until_done = &job;
            let deadline = Instant::now() + Duration::from_secs(4);
            while Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            kill_tree(&mut child);
        };
        if wait {
            finish();
        } else {
            std::thread::spawn(finish);
        }
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        self.stop(false);
    }
}

fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let mut c = Command::new("taskkill");
        c.args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        no_window(&mut c);
        let _ = c.status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn read_message<R: BufRead>(r: &mut R) -> Option<Value> {
    let mut len = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse::<usize>().ok();
            }
        }
    }
    let mut body = vec![0; len?];
    r.read_exact(&mut body).ok()?;
    Some(serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn reader_loop(
    stdout: impl std::io::Read,
    tx: Sender<Vec<u8>>,
    events: Sender<Event>,
    shared: Arc<Shared>,
    wake: impl Fn(),
) {
    let mut r = BufReader::with_capacity(1 << 16, stdout);
    while let Some(msg) = read_message(&mut r) {
        let method = msg.get("method").and_then(Value::as_str);
        match (method, msg.get("id")) {
            (Some("workspace/applyEdit"), Some(id)) => {
                let ops = parse_workspace_edit(&msg["params"]["edit"]);
                let _ = events.send(Event::ApplyEdit { request: id.clone(), ops });
            }
            (Some(m), Some(id)) => {
                let result = server_request(m, msg.get("params"), &shared);
                let _ = tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "result": result })));
            }
            (Some(m), None) => server_notification(m, msg.get("params"), &events, &shared),
            (None, Some(id)) => handle_response(id.as_u64(), &msg, &tx, &events, &shared),
            (None, None) => {}
        }
        wake();
    }
    if !shared.stopping.load(Ordering::SeqCst) {
        set_status(&shared, Status::Failed("The Java language server stopped.".into()));
    }
    wake();
}

fn set_status(shared: &Shared, status: Status) {
    if let Ok(mut s) = shared.status.lock() {
        *s = status;
    }
}

fn server_request(method: &str, params: Option<&Value>, shared: &Shared) -> Value {
    match method {
        "workspace/configuration" => {
            let items = params.and_then(|p| p["items"].as_array()).map_or(&[][..], Vec::as_slice);
            Value::Array(
                items
                    .iter()
                    .map(|i| match i["section"].as_str() {
                        Some("java") => shared.settings.clone(),
                        Some(s) => s
                            .strip_prefix("java.")
                            .and_then(|rest| rest.split('.').try_fold(&shared.settings, |v, k| v.get(k)))
                            .cloned()
                            .unwrap_or(Value::Null),
                        None => Value::Null,
                    })
                    .collect(),
            )
        }
        "workspace/workspaceFolders" => shared.folders.clone(),
        _ => Value::Null,
    }
}

fn server_notification(method: &str, params: Option<&Value>, events: &Sender<Event>, shared: &Shared) {
    let Some(p) = params else { return };
    match method {
        "textDocument/publishDiagnostics" => {
            if let Some(path) = p["uri"].as_str().and_then(uri_to_path) {
                let items = p["diagnostics"].as_array().map_or_else(Vec::new, |a| a.iter().map(parse_diag).collect());
                let _ = events.send(Event::Diagnostics { path, items });
            }
        }
        "language/status" => {
            let message = p["message"].as_str().unwrap_or("").to_string();
            match p["type"].as_str() {
                Some("ServiceReady") => set_status(shared, Status::Ready),
                Some("Error") => set_status(shared, Status::Failed(message)),
                Some(_) => {
                    if let Ok(mut s) = shared.status.lock() {
                        if matches!(*s, Status::Starting(_)) {
                            *s = Status::Starting(message);
                        }
                    }
                }
                None => {}
            }
        }
        "language/progressReport" => {
            if let Some(m) = p["status"].as_str().or(p["task"].as_str()) {
                if let Ok(mut s) = shared.status.lock() {
                    if matches!(*s, Status::Starting(_)) {
                        *s = Status::Starting(m.to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

fn handle_response(id: Option<u64>, msg: &Value, tx: &Sender<Vec<u8>>, events: &Sender<Event>, shared: &Shared) {
    let Some(id) = id else { return };
    let kind = shared.pending.lock().ok().and_then(|mut p| p.remove(&id));
    let result = msg.get("result").unwrap_or(&Value::Null);
    match kind {
        Some(Pending::Initialize) => {
            if let Some(e) = msg.get("error") {
                let text = e["message"].as_str().unwrap_or("initialize failed").to_string();
                set_status(shared, Status::Failed(text));
                return;
            }
            if let (Some(list), Ok(mut commands)) =
                (result["capabilities"]["executeCommandProvider"]["commands"].as_array(), shared.commands.lock())
            {
                *commands = list.iter().filter_map(|c| c.as_str().map(str::to_string)).collect();
            }
            let _ = tx.send(notification("initialized", json!({})));
            let _ = tx.send(notification(
                "workspace/didChangeConfiguration",
                json!({ "settings": { "java": shared.settings } }),
            ));
            shared.initialized.store(true, Ordering::SeqCst);
        }
        Some(Pending::Completion) => {
            let list = result.as_array().or(result["items"].as_array());
            let mut items: Vec<Item> = list.map_or_else(Vec::new, |a| a.iter().map(parse_item).collect());
            items.sort_by(|a, b| {
                let key = |i: &Item| i.lsp.as_ref().map(|d| d.sort.clone()).unwrap_or_default();
                key(a).cmp(&key(b))
            });
            let _ = events.send(Event::Completion { id, items });
        }
        Some(Pending::Resolve) => {
            let edits = parse_edits(&result["additionalTextEdits"]);
            let _ = events.send(Event::Resolved { id, edits });
        }
        Some(Pending::CodeActions) => {
            let commands = shared.commands.lock().map(|c| c.clone()).unwrap_or_default();
            let _ = events.send(Event::CodeActions { id, items: parse_code_actions(result, &commands) });
        }
        Some(Pending::ActionResolve) => {
            if msg.get("error").is_some() || result.is_null() {
                let _ = events.send(Event::CommandFailed { message: "The quick fix could not be prepared.".into() });
            } else {
                let _ = events.send(Event::ActionResolved { action: result.clone() });
            }
        }
        Some(Pending::Command) => {
            if let Some(e) = msg.get("error") {
                let message = e["message"].as_str().unwrap_or("The command failed.").to_string();
                let _ = events.send(Event::CommandFailed { message });
            }
        }
        None => {}
    }
}

const APPLY_EDIT_COMMAND: &str = "java.apply.workspaceEdit";

fn command_name(action: &Value) -> Option<&str> {
    action["command"].as_str().or(action["command"]["command"].as_str())
}

/// The actions this editor can carry out, fixes first.
fn parse_code_actions(result: &Value, commands: &[String]) -> Vec<CodeAction> {
    let mut items: Vec<CodeAction> = result
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter(|a| a.get("disabled").is_none())
        .filter(|a| {
            a.get("edit").is_some()
                || a.get("data").is_some()
                || command_name(a).is_some_and(|c| c == APPLY_EDIT_COMMAND || commands.iter().any(|k| k == c))
        })
        .map(|a| CodeAction {
            title: a["title"].as_str().unwrap_or("").to_string(),
            kind: a["kind"].as_str().unwrap_or("").to_string(),
            raw: Arc::new(a.clone()),
        })
        .collect();
    let rank = |a: &CodeAction| {
        if a.raw["isPreferred"].as_bool() == Some(true) {
            0
        } else if a.kind.starts_with("quickfix") {
            1
        } else if a.kind.starts_with("refactor") {
            2
        } else {
            3
        }
    };
    items.sort_by_key(rank);
    items
}

fn parse_workspace_edit(edit: &Value) -> Vec<FileOp> {
    let mut ops = Vec::new();
    if let Some(changes) = edit["documentChanges"].as_array() {
        for c in changes {
            let options = &c["options"];
            let overwrite = options["overwrite"].as_bool().unwrap_or(false);
            let ignore = options["ignoreIfExists"].as_bool().unwrap_or(false);
            match c["kind"].as_str() {
                Some("create") => {
                    if let Some(path) = c["uri"].as_str().and_then(uri_to_path) {
                        ops.push(FileOp::Create { path, overwrite, ignore_if_exists: ignore });
                    }
                }
                Some("rename") => {
                    let from = c["oldUri"].as_str().and_then(uri_to_path);
                    let to = c["newUri"].as_str().and_then(uri_to_path);
                    if let (Some(from), Some(to)) = (from, to) {
                        ops.push(FileOp::Rename { from, to, overwrite, ignore_if_exists: ignore });
                    }
                }
                Some("delete") => {
                    if let Some(path) = c["uri"].as_str().and_then(uri_to_path) {
                        let ignore_if_missing = options["ignoreIfNotExists"].as_bool().unwrap_or(false);
                        ops.push(FileOp::Delete { path, ignore_if_missing });
                    }
                }
                _ => {
                    if let Some(path) = c["textDocument"]["uri"].as_str().and_then(uri_to_path) {
                        let version = c["textDocument"]["version"].as_i64();
                        ops.push(FileOp::Edit { path, version, edits: parse_edits(&c["edits"]) });
                    }
                }
            }
        }
    } else if let Some(changes) = edit["changes"].as_object() {
        for (uri, edits) in changes {
            if let Some(path) = uri_to_path(uri) {
                ops.push(FileOp::Edit { path, version: None, edits: parse_edits(edits) });
            }
        }
    }
    ops
}

// ------------------------------------------------------------ protocol values

fn parse_pos(v: &Value) -> Pos {
    (v["line"].as_u64().unwrap_or(0) as usize, v["character"].as_u64().unwrap_or(0) as usize)
}

fn parse_diag(v: &Value) -> Diag {
    Diag {
        start: parse_pos(&v["range"]["start"]),
        end: parse_pos(&v["range"]["end"]),
        severity: v["severity"].as_u64().unwrap_or(1) as u8,
        message: v["message"].as_str().unwrap_or("").to_string(),
        raw: Arc::new(v.clone()),
    }
}

fn parse_edits(v: &Value) -> Vec<TextEdit> {
    v.as_array().map_or_else(Vec::new, |a| {
        a.iter()
            .map(|e| TextEdit {
                start: parse_pos(&e["range"]["start"]),
                end: parse_pos(&e["range"]["end"]),
                text: e["newText"].as_str().unwrap_or("").replace("\r\n", "\n"),
            })
            .collect()
    })
}

fn item_kind(k: u64) -> Kind {
    match k {
        2..=4 => Kind::Method,
        5 | 10 | 20 | 21 => Kind::Field,
        6 => Kind::Variable,
        7 | 8 | 9 | 13 | 22 | 25 => Kind::Class,
        15 => Kind::Snippet,
        _ => Kind::Keyword,
    }
}

pub fn parse_item(v: &Value) -> Item {
    let name = v["label"].as_str().unwrap_or("").to_string();
    let details = &v["labelDetails"];
    let label = format!("{name}{}", details["detail"].as_str().unwrap_or(""));
    let detail = details["description"]
        .as_str()
        .or(v["detail"].as_str())
        .and_then(|d| d.lines().next())
        .unwrap_or("")
        .to_string();
    let edit = v.get("textEdit");
    let text = edit
        .and_then(|e| e["newText"].as_str())
        .or(v["insertText"].as_str())
        .unwrap_or(&name)
        .replace("\r\n", "\n");
    let start = edit.and_then(|e| {
        let range = if e.get("range").is_some() { &e["range"] } else { &e["insert"] };
        range.get("start").map(parse_pos)
    });
    let insert = if v["insertTextFormat"].as_u64() == Some(2) { expand_snippet(&text) } else { text };
    Item {
        insert: insert.replace('\t', "    "),
        detail,
        kind: item_kind(v["kind"].as_u64().unwrap_or(1)),
        lsp: Some(ItemData {
            filter: v["filterText"].as_str().unwrap_or(&name).to_string(),
            sort: v["sortText"].as_str().unwrap_or(&name).to_string(),
            start,
            raw: Arc::new(v.clone()),
        }),
        label,
    }
}

// Tab stops travel through the insert text as private-use characters, so that
// re-indenting the text can't disturb them.
const STOP_OPEN_BASE: u32 = 0xF0000;
const STOP_OPEN_END: u32 = 0xF4000;
const STOP_CLOSE: char = '\u{F4000}';

fn stop_open(n: u32) -> char {
    char::from_u32(STOP_OPEN_BASE + n.min(STOP_OPEN_END - STOP_OPEN_BASE - 1)).unwrap_or(STOP_CLOSE)
}

/// Expand an LSP snippet, keeping its tab stops as markers for `strip_stops`.
pub fn expand_snippet(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let (mut i, mut out) = (0, String::new());
    snippet_into(&chars, &mut i, &mut out, false);
    out
}

/// Remove the markers. Returns the text and each stop as (number, start, end) in chars.
pub fn strip_stops(s: &str) -> (String, Vec<(u32, usize, usize)>) {
    let (mut out, mut n, mut open, mut stops) = (String::with_capacity(s.len()), 0, Vec::new(), Vec::new());
    for c in s.chars() {
        let v = c as u32;
        if (STOP_OPEN_BASE..STOP_OPEN_END).contains(&v) {
            open.push((v - STOP_OPEN_BASE, n));
        } else if c == STOP_CLOSE {
            if let Some((k, a)) = open.pop() {
                stops.push((k, a, n));
            }
        } else {
            out.push(c);
            n += 1;
        }
    }
    while let Some((k, a)) = open.pop() {
        stops.push((k, a, n));
    }
    (out, stops)
}

fn snippet_into(c: &[char], i: &mut usize, out: &mut String, nested: bool) {
    while *i < c.len() {
        match c[*i] {
            '\\' if c.get(*i + 1).is_some_and(|n| matches!(n, '$' | '}' | '\\')) => {
                out.push(c[*i + 1]);
                *i += 2;
            }
            '}' if nested => {
                *i += 1;
                return;
            }
            '$' => {
                *i += 1;
                let braced = c.get(*i) == Some(&'{');
                if braced {
                    *i += 1;
                }
                let from = *i;
                while c.get(*i).is_some_and(|n| n.is_alphanumeric() || *n == '_') {
                    *i += 1;
                }
                let name: String = c[from..*i].iter().collect();
                if name.is_empty() && !braced {
                    out.push('$');
                    continue;
                }
                let number = name.parse::<u32>().ok();
                if let Some(n) = number {
                    out.push(stop_open(n));
                }
                if braced {
                    match c.get(*i) {
                        Some(':') => {
                            *i += 1;
                            snippet_into(c, i, out, true);
                        }
                        Some('|') => {
                            *i += 1;
                            while let Some(&n) = c.get(*i) {
                                if n == ',' || n == '|' {
                                    break;
                                }
                                out.push(n);
                                *i += 1;
                            }
                            while c.get(*i).is_some_and(|n| *n != '}') {
                                *i += 1;
                            }
                            *i += 1;
                        }
                        _ => {
                            while c.get(*i).is_some_and(|n| *n != '}') {
                                *i += 1;
                            }
                            *i += 1;
                        }
                    }
                }
                if number.is_some() {
                    out.push(STOP_CLOSE);
                }
            }
            ch => {
                out.push(ch);
                *i += 1;
            }
        }
    }
}

// ------------------------------------------------------------ paths and positions

pub fn path_to_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if s.starts_with('/') { "file://" } else { "file:///" });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file:")?.trim_start_matches('/');
    let b = rest.as_bytes();
    let mut bytes = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%').then(|| rest.get(i + 1..i + 3)).flatten();
        match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(v) => {
                bytes.push(v);
                i += 3;
            }
            None => {
                bytes.push(b[i]);
                i += 1;
            }
        }
    }
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    if cfg!(windows) {
        let mut s = decoded.replace('/', "\\");
        if s.as_bytes().get(1) == Some(&b':') {
            s[..1].make_ascii_uppercase();
        }
        Some(PathBuf::from(s))
    } else {
        Some(PathBuf::from(format!("/{decoded}")))
    }
}

/// A comparable form of a path (Windows paths are case-insensitive).
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
}

pub fn byte_of_pos(text: &str, (line, col): Pos) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let mut units = 0;
    for (i, ch) in text[start..end].char_indices() {
        if units >= col {
            return start + i;
        }
        units += ch.len_utf16();
    }
    end
}

pub fn pos_of_byte(text: &str, byte: usize) -> Pos {
    let before = &text[..byte.min(text.len())];
    let line = before.matches('\n').count();
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, before[start..].encode_utf16().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_expand_with_their_tab_stops() {
        let parsed = |s: &str| {
            let (text, mut stops) = strip_stops(&expand_snippet(s));
            stops.sort();
            (text, stops)
        };
        assert_eq!(parsed("println($0)"), ("println()".into(), vec![(0, 8, 8)]));
        assert_eq!(parsed("foo(${1:a}, ${2:b})$0"), ("foo(a, b)".into(), vec![(0, 9, 9), (1, 4, 5), (2, 7, 8)]));
        assert_eq!(
            parsed("for (int ${1:i} = 0; ${1:i} < n; ${1:i}++) {\n\t$0\n}").0,
            "for (int i = 0; i < n; i++) {\n\t\n}"
        );
        assert_eq!(parsed("${1|one,two|} x"), ("one x".into(), vec![(1, 0, 3)]));
        assert_eq!(parsed("a\\$b").0, "a$b");
        assert_eq!(parsed("${1:outer ${2:inner}}"), ("outer inner".into(), vec![(1, 0, 11), (2, 6, 11)]));
    }

    #[test]
    fn workspace_edits_and_actions_parse() {
        let edit = json!({ "documentChanges": [
            { "kind": "create", "uri": "file:///F:/p/New.java", "options": { "ignoreIfExists": true } },
            { "textDocument": { "uri": "file:///F:/p/New.java", "version": 3 },
              "edits": [{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }, "newText": "class New {}\r\n" }] },
            { "kind": "rename", "oldUri": "file:///F:/p/A.java", "newUri": "file:///F:/p/B.java" },
            { "kind": "delete", "uri": "file:///F:/p/C.java" }
        ]});
        let ops = parse_workspace_edit(&edit);
        assert_eq!(ops.len(), 4);
        assert!(matches!(&ops[0], FileOp::Create { ignore_if_exists: true, .. }));
        assert!(matches!(&ops[1], FileOp::Edit { version: Some(3), edits, .. } if edits[0].text == "class New {}\n"));
        assert!(matches!(&ops[2], FileOp::Rename { .. }));
        assert!(matches!(&ops[3], FileOp::Delete { ignore_if_missing: false, .. }));

        let actions = json!([
            { "title": "Extract", "kind": "refactor.extract", "edit": {} },
            { "title": "Needs a dialog", "kind": "refactor", "command": { "command": "java.action.prompt" } },
            { "title": "Disabled", "kind": "quickfix", "edit": {}, "disabled": { "reason": "no" } },
            { "title": "Import 'List'", "kind": "quickfix", "data": {} }
        ]);
        let titles: Vec<String> = parse_code_actions(&actions, &[]).into_iter().map(|a| a.title).collect();
        assert_eq!(titles, ["Import 'List'", "Extract"]);
    }

    #[test]
    fn positions_use_utf16_columns() {
        let t = "ab\nč😀x\n";
        let b = byte_of_pos(t, (1, 3));
        assert_eq!(&t[b..b + 1], "x");
        assert_eq!(pos_of_byte(t, b), (1, 3));
        assert_eq!(byte_of_pos(t, (9, 0)), t.len());
        assert_eq!(byte_of_pos(t, (0, 99)), 2);
    }

    #[cfg(windows)]
    #[test]
    fn uris_round_trip() {
        let p = Path::new(r"F:\moj projekt\Šola\Main.java");
        let u = path_to_uri(p);
        assert!(u.starts_with("file:///F:/moj%20projekt/"));
        assert_eq!(uri_to_path(&u).unwrap(), p);
        assert_eq!(path_key(&uri_to_path("file:///f%3A/a/B.java").unwrap()), path_key(Path::new(r"F:\A\b.java")));
    }

    #[test]
    fn items_keep_their_import_data() {
        let v = json!({
            "label": "ArrayList",
            "labelDetails": { "detail": "<E>", "description": "java.util" },
            "kind": 7,
            "sortText": "999",
            "filterText": "ArrayList",
            "insertTextFormat": 2,
            "textEdit": { "newText": "ArrayList<$0>", "range": { "start": { "line": 3, "character": 8 }, "end": { "line": 3, "character": 11 } } }
        });
        let i = parse_item(&v);
        assert_eq!(i.label, "ArrayList<E>");
        assert_eq!(i.detail, "java.util");
        assert_eq!(i.kind, Kind::Class);
        assert_eq!(strip_stops(&i.insert), ("ArrayList<>".to_string(), vec![(0, 10, 10)]));
        assert_eq!(i.lsp.unwrap().start, Some((3, 8)));
    }

    /// Runs the real server: `JAVAIDE_JDTLS=<server folder> cargo test -- --ignored real_server`.
    #[test]
    #[ignore]
    fn real_server_completes_imports_and_reports_errors() {
        let root = std::env::temp_dir().join(format!("javaide-lsp-test-{}", std::process::id()));
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        exercise_server(&root, &src.join("Main.java"), false);
    }

    /// A file opened on its own, in a folder that is not a project.
    #[test]
    #[ignore]
    fn real_server_handles_a_single_file() {
        let root = std::env::temp_dir().join(format!("javaide-lsp-single-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        exercise_server(&root, &root.join("Main.java"), true);
    }

    fn exercise_server(root: &Path, file: &Path, single_file: bool) {
        let server = find_server().expect("set JAVAIDE_JDTLS to the server folder");
        let jdk = Jdk::detect(None);
        assert!(jdk.major >= MIN_JAVA, "needs Java {MIN_JAVA}+");
        let file = file.to_path_buf();
        let text = "public class Main {\n    public static void main(String[] args) {\n        ArrayLi\n    }\n}\n";
        std::fs::write(&file, text).unwrap();

        let mut lsp = Lsp::start(&server, &jdk, root, single_file, || {}).unwrap();
        fn wait_until(lsp: &mut Lsp, file: &Path, text: &str, secs: u64, mut done: impl FnMut(Event) -> bool) -> bool {
            let end = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < end {
                lsp.sync(std::iter::once((file, text)));
                for ev in lsp.poll() {
                    if done(ev) {
                        return true;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        }
        let end = Instant::now() + Duration::from_secs(150);
        while !lsp.is_ready() && Instant::now() < end {
            lsp.sync(std::iter::once((file.as_path(), text)));
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(lsp.status(), Status::Ready);

        let key = path_key(&file);
        let saw_error = wait_until(&mut lsp, &file, text, 90, |ev| {
            matches!(ev, Event::Diagnostics { ref path, ref items } if path_key(path) == key && items.iter().any(|d| d.severity == 1))
        });
        assert!(saw_error, "no live error for the unfinished statement");

        let byte = text.find("ArrayLi").unwrap() + "ArrayLi".len();
        let id = lsp.request_completion(&file, pos_of_byte(text, byte), None).unwrap();
        let mut found: Option<Item> = None;
        let got = wait_until(&mut lsp, &file, text, 60, |ev| {
            if let Event::Completion { id: i, items } = ev {
                if i == id {
                    found = items.into_iter().find(|it| it.label.starts_with("ArrayList") && it.detail.contains("java.util"));
                    return true;
                }
            }
            false
        });
        assert!(got, "no completion response");
        let item = found.expect("java.util.ArrayList not suggested");
        let rid = lsp.resolve(&item.lsp.as_ref().unwrap().raw);
        let mut edits = Vec::new();
        let resolved = wait_until(&mut lsp, &file, text, 30, |ev| {
            if let Event::Resolved { id, edits: e } = ev {
                if id == rid {
                    edits = e;
                    return true;
                }
            }
            false
        });
        assert!(resolved, "no resolve response");
        assert!(edits.iter().any(|e| e.text.contains("import java.util.ArrayList;")), "no import edit: {edits:?}");

        lsp.stop(true);
        let _ = std::fs::remove_dir_all(root);
    }
}
