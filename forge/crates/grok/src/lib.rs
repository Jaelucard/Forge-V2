use forge_domain::{PermissionOption, PermissionQuestion, ProviderStatus, Usage, UserQuestion};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
    time::{Instant, timeout},
};

const MAX_FRAME: usize = 1024 * 1024;
const MAX_PROMPT: usize = 256 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct GrokRunConfig {
    pub executable: PathBuf,
    pub workspace: PathBuf,
    pub prompt: String,
    pub session_id: Option<String>,
}

#[derive(Debug)]
pub enum Control {
    Cancel,
    Permission {
        request_id: String,
        option_id: String,
    },
    UserInput {
        request_id: String,
        answers: HashMap<String, Vec<String>>,
    },
}

#[derive(Clone, Debug)]
pub enum Event {
    Started { pid: u32 },
    Session(String),
    Text { role: String, delta: String },
    Permission(PermissionQuestion),
    Usage(Usage),
}

#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub status: String,
    pub error: Option<String>,
    pub usage: Usage,
}

pub async fn probe(configured: &str) -> ProviderStatus {
    let mut status = ProviderStatus {
        message: "Install Grok Build and sign in, then choose its executable in Settings.".into(),
        auth_status: "not_checked".into(),
        fan_out: "Four Forge runs maximum; Grok's own subagents and detached processes are not capped by Forge.".into(),
        workspace_authority: "The project is task context. Grok runs with your OS account access; it is not confined to the project folder.".into(),
        ..Default::default()
    };
    let Some(executable) = discover(configured) else {
        return status;
    };
    status.executable = Some(executable.to_string_lossy().into_owned());
    let mut command = Command::new(&executable);
    command
        .arg("--version")
        .env_remove("XAI_API_KEY")
        .env_remove("GROK_CODE_XAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        status.message = "The selected Grok program could not be started.".into();
        return status;
    };
    let Some(stdout) = child.stdout.take() else {
        return status;
    };
    let version_result = timeout(Duration::from_secs(3), async {
        let mut data = Vec::new();
        stdout
            .take(4097)
            .read_to_end(&mut data)
            .await
            .map_err(|_| ())?;
        if data.len() > 4096 {
            return Err(());
        }
        let code = child.wait().await.map_err(|_| ())?;
        if !code.success() {
            return Err(());
        }
        let version = String::from_utf8(data).map_err(|_| ())?;
        let version = version.trim();
        if version.is_empty()
            || version
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(());
        }
        Ok(version.chars().take(256).collect::<String>())
    })
    .await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    match version_result {
        Ok(Ok(version)) => {
            status.available = true;
            status.version = Some(version);
            status.message = "Grok Build is available. Your saved Grok account is checked when you send a message.".into();
        }
        _ => {
            status.message =
                "The selected program did not return a valid Grok version. Check its path.".into()
        }
    }
    status
}

fn discover(configured: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = if let Some(relative) = configured.strip_prefix("~/") {
            PathBuf::from(std::env::var_os("HOME")?).join(relative)
        } else {
            PathBuf::from(configured)
        };
        if path.components().count() > 1 || path.is_absolute() {
            candidates.push(path);
        } else if let Some(paths) = std::env::var_os("PATH") {
            candidates.extend(std::env::split_paths(&paths).map(|p| p.join(&path)));
        }
    } else {
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".grok/bin/grok"));
        }
        if let Some(paths) = std::env::var_os("PATH") {
            candidates.extend(
                std::env::split_paths(&paths)
                    .map(|p| p.join(if cfg!(windows) { "grok.exe" } else { "grok" })),
            );
        }
    }
    candidates
        .into_iter()
        .find(|path| executable_file(path))
        .and_then(|p| p.canonicalize().ok())
}

fn executable_file(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub async fn run(
    config: GrokRunConfig,
    controls: mpsc::Receiver<Control>,
    events: mpsc::Sender<Event>,
) -> Result<RunOutcome, String> {
    if config.prompt.trim().is_empty() || config.prompt.len() > MAX_PROMPT {
        return Err("The message must contain text and be smaller than 256 KiB.".into());
    }
    let workspace = config
        .workspace
        .canonicalize()
        .map_err(|_| "The project folder is unavailable.")?;
    if !workspace.is_dir() {
        return Err("The project path must be a folder.".into());
    }
    if !executable_file(&config.executable) {
        return Err("Grok Build is not available at the configured path.".into());
    }
    let mut command = Command::new(&config.executable);
    command
        .args(["agent", "--always-approve", "--no-leader", "stdio"])
        .current_dir(&workspace)
        .env_remove("XAI_API_KEY")
        .env_remove("GROK_CODE_XAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| "Grok Build could not be started. Check its executable and permissions.")?;
    let pid = child
        .id()
        .ok_or("Grok Build did not provide a process ID.")?;
    let stdin = child
        .stdin
        .take()
        .ok_or("Grok Build stdin is unavailable.")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Grok Build stdout is unavailable.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Grok Build stderr is unavailable.")?;
    let (frame_tx, frame_rx) = mpsc::channel(8);
    let reader = tokio::spawn(read_frames(BufReader::new(stdout), frame_tx));
    let stderr_reader = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut BufReader::new(stderr), &mut tokio::io::sink()).await;
    });
    let mut client = Client {
        stdin,
        frames: frame_rx,
        controls,
        events,
        next_id: 1,
        session_id: None,
        replaying: false,
        prompt_sent: false,
        cancelled: false,
        cancel_deadline: None,
        permissions: HashMap::new(),
        user_inputs: HashMap::new(),
        usage: Usage::default(),
    };
    let result = async {
        client.emit(Event::Started { pid }).await?;
        client.perform(&config, &workspace).await
    }
    .await;
    let outcome = match result {
        Ok(reason) => RunOutcome {
            status: if client.cancelled || reason == "cancelled" {
                "cancelled"
            } else if reason == "end_turn" {
                "completed"
            } else {
                "failed"
            }
            .into(),
            error: if matches!(reason.as_str(), "end_turn" | "cancelled") {
                None
            } else {
                Some(format!(
                    "Grok stopped before completing this turn ({reason})."
                ))
            },
            usage: client.usage.clone(),
        },
        Err(error) => RunOutcome {
            status: if client.prompt_sent {
                "unknown"
            } else if client.cancelled {
                "cancelled"
            } else {
                "failed"
            }
            .into(),
            error: Some(error),
            usage: client.usage.clone(),
        },
    };
    let _ = client.stdin.shutdown().await;
    drop(client);
    let stopped = stop_owned(&mut child).await;
    reader.abort();
    stderr_reader.abort();
    if !stopped {
        return Ok(RunOutcome { status: "unknown".into(), error: Some("Grok's process could not be confirmed stopped. The project remains locked until you reconcile this run.".into()), usage: outcome.usage });
    }
    Ok(outcome)
}

async fn stop_owned(child: &mut Child) -> bool {
    let pid = child.id();
    if let Ok(Ok(_)) = timeout(Duration::from_secs(1), child.wait()).await {
        return owned_group_gone(pid);
    }
    #[cfg(unix)]
    if matches!(child.try_wait(), Ok(None))
        && let Some(pid) = child.id()
    {
        // The unreaped child owns this fresh process group, so its ID cannot be reused here.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    if let Ok(Ok(_)) = timeout(Duration::from_secs(2), child.wait()).await {
        return owned_group_gone(pid);
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
    matches!(
        timeout(Duration::from_secs(2), child.wait()).await,
        Ok(Ok(_))
    ) && owned_group_gone(pid)
}

fn owned_group_gone(pid: Option<u32>) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = pid else { return false };
        // After reaping, probe only; never signal a group whose leader identity is lost.
        let result = unsafe { libc::kill(-(pid as i32), 0) };
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

async fn read_frames<R: tokio::io::AsyncBufRead + Unpin>(
    mut reader: R,
    sender: mpsc::Sender<Result<Value, String>>,
) {
    let mut line = Vec::new();
    loop {
        let buffer = match reader.fill_buf().await {
            Ok(buffer) => buffer,
            Err(_) => {
                let _ = sender
                    .send(Err("Grok's output stream could not be read.".into()))
                    .await;
                return;
            }
        };
        if buffer.is_empty() {
            if !line.is_empty() {
                let _ = sender
                    .send(Err("Grok ended an incomplete protocol message.".into()))
                    .await;
            }
            return;
        }
        let end = buffer.iter().position(|byte| *byte == b'\n');
        let take = end.map_or(buffer.len(), |at| at + 1);
        if line.len() + take > MAX_FRAME {
            let _ = sender
                .send(Err(
                    "Grok sent a protocol message larger than the 1 MiB limit.".into(),
                ))
                .await;
            return;
        }
        line.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        if end.is_some() {
            if !line.iter().all(|b| b.is_ascii_whitespace()) {
                let parsed = serde_json::from_slice(&line)
                    .map_err(|_| "Grok sent an invalid protocol message.".into());
                let invalid = parsed.is_err();
                if sender.send(parsed).await.is_err() || invalid {
                    return;
                }
            }
            line.clear();
        }
    }
}

struct Client {
    stdin: ChildStdin,
    frames: mpsc::Receiver<Result<Value, String>>,
    controls: mpsc::Receiver<Control>,
    events: mpsc::Sender<Event>,
    next_id: u64,
    session_id: Option<String>,
    replaying: bool,
    prompt_sent: bool,
    cancelled: bool,
    cancel_deadline: Option<Instant>,
    permissions: HashMap<String, (Value, Vec<String>)>,
    user_inputs: HashMap<String, (Value, Vec<Value>)>,
    usage: Usage,
}

impl Client {
    async fn emit(&self, event: Event) -> Result<(), String> {
        timeout(EVENT_TIMEOUT, self.events.send(event))
            .await
            .map_err(|_| "Forge could not save Grok output in time.")?
            .map_err(|_| "Forge stopped receiving Grok output.".into())
    }

    async fn write(&mut self, value: Value) -> Result<(), String> {
        let mut data =
            serde_json::to_vec(&value).map_err(|_| "Could not encode a Grok request.")?;
        if data.len() > MAX_FRAME {
            return Err("The Grok request exceeds the 1 MiB limit.".into());
        }
        data.push(b'\n');
        timeout(EVENT_TIMEOUT, async {
            self.stdin.write_all(&data).await?;
            self.stdin.flush().await
        })
        .await
        .map_err(|_| "Grok stopped accepting requests.")?
        .map_err(|_| "The Grok input stream closed.".into())
    }

    async fn perform(
        &mut self,
        config: &GrokRunConfig,
        workspace: &Path,
    ) -> Result<String, String> {
        let initialize = self.call("initialize", json!({"protocolVersion":1,"clientInfo":{"name":"forge-v2","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}), true).await?;
        if initialize.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
            return Err("This Grok Build protocol version is not supported by Forge yet.".into());
        }
        require_cached_auth(&initialize)?;
        self.call("authenticate", json!({"methodId":"cached_token"}), true).await.map_err(|_| "Grok account sign-in is required. Run `grok login` with this installation, then retry.".to_string())?;
        let session = if let Some(session_id) = &config.session_id {
            if initialize
                .pointer("/agentCapabilities/loadSession")
                .and_then(Value::as_bool)
                != Some(true)
            {
                return Err("This Grok version cannot restore the saved session. Start a new conversation explicitly.".into());
            }
            self.session_id = Some(session_id.clone());
            self.replaying = true;
            let loaded = self
                .call(
                    "session/load",
                    json!({"sessionId":session_id,"cwd":workspace,"mcpServers":[]}),
                    true,
                )
                .await;
            self.replaying = false;
            loaded.map_err(|_| {
                "Grok could not restore this saved session. No new prompt was sent.".to_string()
            })?;
            session_id.clone()
        } else {
            let result = self
                .call(
                    "session/new",
                    json!({"cwd":workspace,"mcpServers":[],"_meta":{"yoloMode":true}}),
                    true,
                )
                .await?;
            result
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() < 4096)
                .ok_or("Grok did not provide a valid session ID.")?
                .to_owned()
        };
        self.session_id = Some(session.clone());
        self.emit(Event::Session(session.clone())).await?;
        if self.cancelled {
            return Err("The run was cancelled before submission.".into());
        }
        self.prompt_sent = true;
        let result = self
            .call(
                "session/prompt",
                json!({"sessionId":session,"prompt":[{"type":"text","text":config.prompt}]}),
                false,
            )
            .await?;
        Ok(result
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .chars()
            .take(80)
            .collect())
    }

    async fn call(&mut self, method: &str, params: Value, bounded: bool) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let request_deadline = if bounded {
            Instant::now() + HANDSHAKE_TIMEOUT
        } else {
            Instant::now() + Duration::from_secs(24 * 60 * 60)
        };
        loop {
            let deadline = self
                .cancel_deadline
                .map_or(request_deadline, |d| d.min(request_deadline));
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return Err(if self.cancelled { "Grok did not acknowledge cancellation. Check for remaining tools before resolving this run." } else { "Grok did not answer the protocol request in time." }.into()),
                control = self.controls.recv() => {
                    match control {
                        Some(Control::Cancel) | None => {
                            if !self.cancelled {
                                self.cancelled = true;
                                self.cancel_deadline = Some(Instant::now() + CANCEL_TIMEOUT);
                                if let Some(session) = self.session_id.clone() {
                                    self.write(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session,"_meta":{"cancelTrigger":"ctrl_c","cancelSubagents":true}}})).await?;
                                }
                                self.cancel_permissions().await?;
                            }
                            if !self.prompt_sent || control.is_none() { return Err("Forge closed or cancelled the run; delivery may need reconciliation.".into()); }
                        }
                        Some(Control::Permission { request_id, option_id }) => {
                            if let Some((native_id, options)) = self.permissions.get(&request_id) {
                                if !options.contains(&option_id) { return Err("The selected Grok permission option is invalid.".into()); }
                                let native_id = native_id.clone();
                                self.write(json!({"jsonrpc":"2.0","id":native_id,"result":{"outcome":{"outcome":"selected","optionId":option_id}}})).await?;
                                self.permissions.remove(&request_id);
                            }
                        }
                        Some(Control::UserInput { request_id, answers }) => {
                            if let Some((native_id, questions)) = self.user_inputs.get(&request_id) {
                                let result = normalize_user_answers(questions, &answers)?;
                                let native_id = native_id.clone();
                                self.write(json!({"jsonrpc":"2.0","id":native_id,"result":result})).await?;
                                self.user_inputs.remove(&request_id);
                            }
                        }
                    }
                }
                frame = self.frames.recv() => {
                    let frame = frame.ok_or("Grok exited before acknowledging this request.")??;
                    if frame.get("jsonrpc").and_then(Value::as_str) != Some("2.0") { return Err("Grok returned an unsupported protocol envelope.".into()); }
                    if let Some(method) = frame.get("method").and_then(Value::as_str) {
                        let method = method.to_owned();
                        if frame.get("id").is_some() { self.reverse_request(&method, &frame).await?; }
                        else { self.notification(&method, &frame).await?; }
                    } else if frame.get("id") == Some(&json!(id)) {
                        if let Some(error) = frame.get("error") {
                            let code = error.get("code").and_then(Value::as_i64).unwrap_or(-1);
                            return Err(format!("Grok rejected {method} (error {code}). Check the provider's sign-in and current session."));
                        }
                        return frame.get("result").cloned().ok_or_else(|| "Grok returned a response without a result.".into());
                    } else { return Err("Grok returned a response with an unexpected request ID.".into()); }
                }
            }
        }
    }

    async fn cancel_permissions(&mut self) -> Result<(), String> {
        let ids: Vec<Value> = self.permissions.drain().map(|(_, (id, _))| id).collect();
        for id in ids {
            self.write(
                json!({"jsonrpc":"2.0","id":id,"result":{"outcome":{"outcome":"cancelled"}}}),
            )
            .await?;
        }
        let ids: Vec<Value> = self.user_inputs.drain().map(|(_, (id, _))| id).collect();
        for id in ids {
            self.write(json!({"jsonrpc":"2.0","id":id,"result":{"outcome":"cancelled"}}))
                .await?;
        }
        Ok(())
    }

    async fn reverse_request(&mut self, method: &str, frame: &Value) -> Result<(), String> {
        let id = frame
            .get("id")
            .cloned()
            .ok_or("Missing Grok question ID.")?;
        if !id.is_string() && !id.is_number() {
            return Err("Grok sent an invalid question ID.".into());
        }
        if matches!(method, "x.ai/ask_user_question" | "_x.ai/ask_user_question") {
            return self.user_input_request(id, frame).await;
        }
        if method != "session/request_permission" {
            self.write(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"This client capability is not supported."}})).await?;
            return Ok(());
        }
        let params = frame
            .get("params")
            .ok_or("Grok sent an invalid permission request.")?;
        if params.get("sessionId").and_then(Value::as_str) != self.session_id.as_deref() {
            return Err("Grok requested permission for a different session.".into());
        }
        if self.cancelled || self.replaying {
            self.write(
                json!({"jsonrpc":"2.0","id":id,"result":{"outcome":{"outcome":"cancelled"}}}),
            )
            .await?;
            return Ok(());
        }
        let raw_options = params
            .get("options")
            .and_then(Value::as_array)
            .ok_or("Grok did not provide permission choices.")?;
        if raw_options.is_empty()
            || raw_options.len() > 32
            || self.permissions.len() + self.user_inputs.len() >= 16
        {
            return Err("Grok exceeded the supported permission request limit.".into());
        }
        let options: Vec<PermissionOption> = raw_options
            .iter()
            .map(|option| {
                let id = option
                    .get("optionId")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && s.len() <= 1024)
                    .ok_or("Grok provided an invalid permission choice.")?;
                let label = option
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .chars()
                    .take(240)
                    .collect();
                Ok(PermissionOption {
                    id: id.into(),
                    label,
                    kind: option
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .chars()
                        .take(80)
                        .collect(),
                })
            })
            .collect::<Result<_, String>>()?;
        let request_id = id.to_string();
        if self.permissions.contains_key(&request_id) || self.user_inputs.contains_key(&request_id)
        {
            return Err("Grok reused a pending permission request ID.".into());
        }
        self.permissions.insert(
            request_id.clone(),
            (id, options.iter().map(|o| o.id.clone()).collect()),
        );
        self.emit(Event::Permission(PermissionQuestion {
            kind: "permission".into(),
            questions: Vec::new(),
            run_id: String::new(),
            request_id,
            title: params
                .pointer("/toolCall/title")
                .and_then(Value::as_str)
                .unwrap_or("Grok needs your permission")
                .chars()
                .take(500)
                .collect(),
            options,
        }))
        .await
    }

    async fn user_input_request(&mut self, id: Value, frame: &Value) -> Result<(), String> {
        let raw = frame
            .get("params")
            .ok_or("Grok sent an empty question request.")?;
        let params = raw.get("params").unwrap_or(raw);
        if params.get("sessionId").and_then(Value::as_str) != self.session_id.as_deref() {
            return Err("Grok requested input for a different session.".into());
        }
        if self.cancelled || self.replaying {
            self.write(json!({"jsonrpc":"2.0","id":id,"result":{"outcome":"cancelled"}}))
                .await?;
            return Ok(());
        }
        let native = params
            .get("questions")
            .and_then(Value::as_array)
            .ok_or("Grok did not provide its questions.")?;
        let questions = parse_user_questions(native)?;
        let request_id = id.to_string();
        if self.permissions.contains_key(&request_id)
            || self.user_inputs.contains_key(&request_id)
            || self.permissions.len() + self.user_inputs.len() >= 16
        {
            return Err(
                "Grok reused a pending question ID or exceeded the pending question limit.".into(),
            );
        }
        self.user_inputs
            .insert(request_id.clone(), (id, native.clone()));
        self.emit(Event::Permission(PermissionQuestion {
            kind: "user_input".into(),
            run_id: String::new(),
            request_id,
            title: "Grok needs your input".into(),
            options: Vec::new(),
            questions,
        }))
        .await
    }

    async fn notification(&mut self, method: &str, frame: &Value) -> Result<(), String> {
        if method != "session/update" && method != "x.ai/session/update" {
            return Ok(());
        }
        if self.replaying {
            return Ok(());
        }
        let params = frame
            .get("params")
            .ok_or("Grok sent an empty session update.")?;
        if params.get("sessionId").and_then(Value::as_str) != self.session_id.as_deref() {
            return Ok(());
        }
        let update = params
            .get("update")
            .ok_or("Grok sent an invalid session update.")?;
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                    self.emit(Event::Text {
                        role: "assistant".into(),
                        delta: text.into(),
                    })
                    .await?;
                }
            }
            Some("tool_call" | "tool_call_update") => {
                let tool_id = update
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let title = update
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Tool activity");
                let status = update
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("reported");
                let mut text = format!("\n[{tool_id}] {title} · {status}\n");
                if let Some(content) = update.get("content").and_then(Value::as_array) {
                    for item in content.iter().take(32) {
                        if let Some(part) = item.pointer("/content/text").and_then(Value::as_str) {
                            text.extend(part.chars().take(16_384));
                            text.push('\n');
                        }
                    }
                }
                self.emit(Event::Text {
                    role: "tool".into(),
                    delta: text,
                })
                .await?;
            }
            Some("plan") => {
                if let Some(entries) = update.get("entries").and_then(Value::as_array) {
                    let mut text = String::from("\nGrok's reported plan\n");
                    for entry in entries.iter().take(100) {
                        let status = entry
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("pending");
                        let content = entry.get("content").and_then(Value::as_str).unwrap_or("");
                        text.push_str(&format!(
                            "• {status}: {}\n",
                            content.chars().take(2000).collect::<String>()
                        ));
                    }
                    self.emit(Event::Text {
                        role: "tool".into(),
                        delta: text,
                    })
                    .await?;
                }
            }
            Some("turn_completed" | "usage_update") => {
                if let Some(usage) = reported_usage(update) {
                    self.usage = usage.clone();
                    self.emit(Event::Usage(usage)).await?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn require_cached_auth(initialize: &Value) -> Result<(), String> {
    let cached = initialize
        .get("authMethods")
        .and_then(Value::as_array)
        .is_some_and(|methods| {
            methods
                .iter()
                .any(|m| m.get("id").and_then(Value::as_str) == Some("cached_token"))
        });
    let selected = initialize
        .pointer("/_meta/defaultAuthMethodId")
        .and_then(Value::as_str);
    if !cached || selected.is_some_and(|method| method != "cached_token") {
        return Err("Sign in to this Grok Build installation first. Forge requires your saved Grok account and will not switch to API-key billing.".into());
    }
    Ok(())
}

fn reported_usage(update: &Value) -> Option<Usage> {
    let data = update.get("usage").unwrap_or(update);
    let input_tokens = data.get("inputTokens").and_then(Value::as_u64);
    let output_tokens = data.get("outputTokens").and_then(Value::as_u64);
    let cost_usd = data
        .get("costUsdTicks")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| v / 10_000_000_000.0);
    if input_tokens.is_none() && output_tokens.is_none() && cost_usd.is_none() {
        None
    } else {
        Some(Usage {
            input_tokens,
            output_tokens,
            cost_usd,
        })
    }
}

fn parse_user_questions(native: &[Value]) -> Result<Vec<UserQuestion>, String> {
    if native.is_empty() || native.len() > 10 {
        return Err("Grok must ask between one and ten questions at a time.".into());
    }
    let mut ids = std::collections::HashSet::new();
    let mut texts = std::collections::HashSet::new();
    native
        .iter()
        .map(|q| {
            let question = q
                .get("question")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty() && s.len() <= 4000)
                .ok_or("Grok sent an invalid question.")?;
            let id = q.get("id").and_then(Value::as_str).unwrap_or(question);
            if id.is_empty() || id.len() > 4000 || !ids.insert(id) || !texts.insert(question) {
                return Err("Grok sent ambiguous or duplicate question identifiers.".into());
            }
            let raw_options = q
                .get("options")
                .and_then(Value::as_array)
                .ok_or("Grok sent invalid question choices.")?;
            if raw_options.len() > 32 {
                return Err("Grok sent too many question choices.".into());
            }
            let mut labels = std::collections::HashSet::new();
            let options = raw_options
                .iter()
                .map(|option| {
                    let label = option
                        .get("label")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty() && s.len() <= 4000)
                        .ok_or("Grok sent an invalid question choice.")?;
                    if !labels.insert(label) {
                        return Err("Grok sent duplicate question choices.".to_string());
                    }
                    Ok(label.to_owned())
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(UserQuestion {
                id: id.into(),
                question: question.into(),
                options,
                multi_select: q
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn normalize_user_answers(
    native: &[Value],
    answers: &HashMap<String, Vec<String>>,
) -> Result<Value, String> {
    let questions = parse_user_questions(native)?;
    if answers.len() != questions.len()
        || answers
            .keys()
            .any(|id| !questions.iter().any(|q| &q.id == id))
    {
        return Err("Answer each question in the current Grok request.".into());
    }
    let mut selected = serde_json::Map::new();
    let mut annotations = serde_json::Map::new();
    for (question, raw) in questions.iter().zip(native) {
        let values = &answers[&question.id];
        if values.is_empty()
            || values.len() > 33
            || values.iter().any(|v| v.trim().is_empty() || v.len() > 4000)
        {
            return Err("Grok answers must contain text within the supported size limit.".into());
        }
        let mut labels = Vec::new();
        let mut notes = Vec::new();
        for value in values {
            if question.options.contains(value) {
                if !labels.contains(value) {
                    labels.push(value.clone());
                }
            } else {
                notes.push(value.trim().to_owned());
            }
        }
        if !question.multi_select && labels.len() > 1 {
            return Err("Select one option for this Grok question.".into());
        }
        let mut annotation = serde_json::Map::new();
        if !question.multi_select
            && labels.len() == 1
            && let Some(preview) = raw
                .get("options")
                .and_then(Value::as_array)
                .and_then(|options| {
                    options
                        .iter()
                        .find(|o| o.get("label").and_then(Value::as_str) == Some(&labels[0]))
                })
                .and_then(|o| o.get("preview"))
                .and_then(Value::as_str)
                .filter(|preview| preview.len() <= 16_384)
        {
            annotation.insert("preview".into(), json!(preview));
        }
        if !notes.is_empty() {
            annotation.insert("notes".into(), json!(notes.join("\n")));
        }
        if !annotation.is_empty() {
            annotations.insert(question.question.clone(), Value::Object(annotation));
        }
        selected.insert(
            question.question.clone(),
            if labels.is_empty() {
                json!(["Other"])
            } else {
                json!(labels)
            },
        );
    }
    let mut response = json!({"outcome":"accepted","answers":selected});
    if !annotations.is_empty() {
        response["annotations"] = json!(annotations);
    }
    Ok(response)
}

#[cfg(test)]
mod tests;
