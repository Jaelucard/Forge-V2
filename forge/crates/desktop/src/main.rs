#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod workspace;

use forge_core::Store;
use forge_domain::{PermissionQuestion, Reply, Request, ResourceReport, Run};
use forge_grok::{Control, Event, GrokRunConfig};
use fs2::FileExt;
use std::{
    collections::HashMap,
    fs::File,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{Emitter, Manager};
use tokio::sync::{Notify, mpsc};

struct Runtime {
    control: mpsc::Sender<Control>,
    pid: Option<u32>,
    questions: HashMap<String, PermissionQuestion>,
}

struct Host {
    store: Mutex<Store>,
    runtimes: Mutex<HashMap<String, Runtime>>,
    storage_faults: Mutex<HashMap<String, String>>,
    wake: Notify,
    finished: Notify,
    closing: AtomicBool,
    exiting: AtomicBool,
    admission: tokio::sync::Mutex<()>,
    _instance_lock: File,
}

impl Host {
    fn apply_storage_faults(&self, snapshot: &mut forge_domain::Snapshot) -> Result<(), String> {
        let faults = self
            .storage_faults
            .lock()
            .map_err(|_| "Local storage status is unavailable.")?;
        for run in &mut snapshot.runs {
            if let Some(error) = faults.get(&run.id) {
                run.status = "unknown".into();
                run.error = Some(error.clone());
            }
        }
        Ok(())
    }

    async fn save_outcome(
        self: &Arc<Self>,
        run_id: &str,
        outcome: Result<forge_grok::RunOutcome, String>,
    ) -> Result<(), String> {
        let id = run_id.to_string();
        let result = self
            .db(move |s| match outcome {
                Ok(outcome) => s.finish_run(
                    &id,
                    &outcome.status,
                    outcome.error.as_deref(),
                    Some(outcome.usage),
                ),
                Err(error) => s.finish_run(&id, "unknown", Some(&error), None),
            })
            .await;
        if let Err(error) = &result {
            let message = format!(
                "Local storage could not save this run's final state. Its workspace remains reserved. Restore free disk space, check Grok has stopped, then resolve this run. {error}"
            );
            self.storage_faults
                .lock()
                .map_err(|_| "Local storage status is unavailable.")?
                .insert(run_id.to_string(), message);
        }
        result
    }
    async fn db<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&mut Store) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let host = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = host
                .store
                .lock()
                .map_err(|_| "Local storage is unavailable.")?;
            f(&mut store)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn admit(self: &Arc<Self>) -> Result<Option<(Run, mpsc::Receiver<Control>)>, String> {
        let _admission = self.admission.lock().await;
        if self.closing.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let Some(run) = self.db(|s| s.next_runnable()).await? else {
            return Ok(None);
        };
        let (tx, rx) = mpsc::channel(16);
        self.runtimes
            .lock()
            .map_err(|_| "Run controls are unavailable.")?
            .insert(
                run.id.clone(),
                Runtime {
                    control: tx,
                    pid: None,
                    questions: HashMap::new(),
                },
            );
        Ok(Some((run, rx)))
    }

    async fn cancel(self: &Arc<Self>, run_id: String) -> Result<Run, String> {
        let _admission = self.admission.lock().await;
        let id = run_id.clone();
        let run = self.db(move |s| s.cancel_run(&id)).await?;
        let control = self
            .runtimes
            .lock()
            .map_err(|_| "Run controls are unavailable.")?
            .get(&run_id)
            .map(|r| r.control.clone());
        if let Some(control) = control {
            control
                .send(Control::Cancel)
                .await
                .map_err(|_| "The run is stopping. Refresh to see its final state.")?;
        }
        self.wake.notify_one();
        Ok(run)
    }
}

fn trusted_origin(label: &str, origin: &str) -> bool {
    label == "main"
        && matches!(
            origin,
            "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
        )
}

#[tauri::command]
async fn dispatch(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Arc<Host>>,
    request: Request,
) -> Result<Reply, String> {
    let url = window.url().map_err(|e| e.to_string())?;
    let origin = url.origin().ascii_serialization();
    let origin = if url.scheme() == "tauri" && url.host_str() == Some("localhost") {
        "tauri://localhost"
    } else {
        origin.as_str()
    };
    if !trusted_origin(window.label(), origin) {
        return Err("This window cannot access Forge data.".into());
    }
    let host = state.inner().clone();
    if host.closing.load(Ordering::SeqCst) {
        return Err("Forge is stopping active work before closing.".into());
    }
    let reply = match request {
        Request::ProviderStatus => {
            let settings = host.db(|s| s.settings()).await?;
            Reply::ProviderStatus(forge_grok::probe(&settings.grok_path).await)
        }
        Request::PickFolder => {
            let folder = rfd::AsyncFileDialog::new()
                .set_title("Choose a project folder")
                .pick_folder()
                .await;
            Reply::Folder(folder.map(|p| p.path().to_string_lossy().into_owned()))
        }
        Request::Workspace {
            project_id,
            directory,
        } => {
            let project = host.db(move |s| s.project(&project_id)).await?;
            Reply::Workspace(workspace::inspect(PathBuf::from(project.path), directory).await?)
        }
        Request::ReadFile { project_id, path } => {
            let project = host.db(move |s| s.project(&project_id)).await?;
            let path_copy = path.clone();
            let (text, truncated) = tokio::task::spawn_blocking(move || {
                workspace::read_file(&PathBuf::from(project.path), &path_copy)
            })
            .await
            .map_err(|e| e.to_string())??;
            Reply::File {
                path,
                text,
                truncated,
            }
        }
        Request::Resources => Reply::Resources(resources(&host).await?),
        Request::ReadOutput {
            run_id,
            role,
            offset,
        } => {
            let id = run_id.clone();
            let stream = role.clone();
            let (text, next_offset) = host
                .db(move |s| s.read_output(&id, &stream, offset))
                .await?;
            Reply::Output {
                run_id,
                role,
                offset,
                text,
                next_offset,
            }
        }
        Request::CancelRun { run_id } => {
            let run = host.cancel(run_id).await?;
            Reply::Saved { id: run.id }
        }
        Request::ResolveRun { run_id } => {
            if host
                .runtimes
                .lock()
                .map_err(|_| "Run controls are unavailable.")?
                .contains_key(&run_id)
            {
                return Err(
                    "This run still has a Forge process. Stop it before resolving it.".into(),
                );
            }
            let id = run_id.clone();
            let fault = host
                .storage_faults
                .lock()
                .map_err(|_| "Local storage status is unavailable.")?
                .get(&run_id)
                .cloned();
            host.db(move |s| {
                if let Some(error) = fault {
                    s.finish_run(&id, "unknown", Some(&error), None)?;
                }
                s.resolve_run(&id)
            })
            .await?;
            host.storage_faults
                .lock()
                .map_err(|_| "Local storage status is unavailable.")?
                .remove(&run_id);
            host.wake.notify_one();
            Reply::Saved { id: run_id }
        }
        Request::AnswerPermission {
            run_id,
            request_id,
            option_id,
        } => {
            let control = {
                let runtimes = host
                    .runtimes
                    .lock()
                    .map_err(|_| "Run controls are unavailable.")?;
                let runtime = runtimes
                    .get(&run_id)
                    .ok_or("This run is no longer waiting.")?;
                let question = runtime
                    .questions
                    .get(&request_id)
                    .ok_or("This question has already been answered.")?;
                if !question.options.iter().any(|o| o.id == option_id) {
                    return Err("Choose one of the options supplied by Grok.".into());
                }
                runtime.control.clone()
            };
            control
                .send(Control::Permission {
                    request_id: request_id.clone(),
                    option_id,
                })
                .await
                .map_err(|_| "The run is no longer connected.")?;
            if let Some(runtime) = host
                .runtimes
                .lock()
                .map_err(|_| "Run controls are unavailable.")?
                .get_mut(&run_id)
            {
                runtime.questions.remove(&request_id);
            }
            Reply::Saved { id: run_id }
        }
        Request::AnswerUserInput {
            run_id,
            request_id,
            answers,
        } => {
            let control = {
                let runtimes = host
                    .runtimes
                    .lock()
                    .map_err(|_| "Run controls are unavailable.")?;
                let runtime = runtimes
                    .get(&run_id)
                    .ok_or("This run is no longer waiting.")?;
                let question = runtime
                    .questions
                    .get(&request_id)
                    .ok_or("This question has already been answered.")?;
                if question.kind != "user_input" {
                    return Err("This request needs a permission choice.".into());
                }
                runtime.control.clone()
            };
            control
                .send(Control::UserInput {
                    request_id: request_id.clone(),
                    answers,
                })
                .await
                .map_err(|_| "The run is no longer connected.")?;
            if let Some(runtime) = host
                .runtimes
                .lock()
                .map_err(|_| "Run controls are unavailable.")?
                .get_mut(&run_id)
            {
                runtime.questions.remove(&request_id);
            }
            Reply::Saved { id: run_id }
        }
        request @ Request::QueueRun { .. } => {
            let settings = host.db(|s| s.settings()).await?;
            let provider = forge_grok::probe(&settings.grok_path).await;
            if !provider.available {
                return Err(provider.message);
            }
            let result = host.db(move |s| s.execute(request)).await?;
            host.wake.notify_one();
            result
        }
        request => host.db(move |s| s.execute(request)).await?,
    };
    let reply = match reply {
        Reply::Snapshot(mut snapshot) => {
            snapshot.questions = host
                .runtimes
                .lock()
                .map_err(|_| "Run controls are unavailable.")?
                .values()
                .flat_map(|r| r.questions.values().cloned())
                .collect();
            host.apply_storage_faults(&mut snapshot)?;
            Reply::Snapshot(snapshot)
        }
        other => other,
    };
    let _ = window.emit("forge-changed", ());
    Ok(reply)
}

async fn resources(host: &Arc<Host>) -> Result<ResourceReport, String> {
    let (active_runs, queued_runs) = host.db(|s| s.run_counts()).await?;
    let pids: Vec<u32> = host
        .runtimes
        .lock()
        .map_err(|_| "Run controls are unavailable.")?
        .values()
        .filter_map(|r| r.pid)
        .collect();
    let app_rss_bytes = rss(&[std::process::id()]).await;
    let provider_rss_bytes = if pids.is_empty() {
        Some(0)
    } else {
        rss(&pids).await
    };
    Ok(ResourceReport { app_rss_bytes, provider_rss_bytes, active_runs, queued_runs, message: "Memory is sampled on request. App figure excludes system WebView helpers; provider figure covers direct Grok processes. Descendants and remote usage may be unreported.".into() })
}

async fn rss(pids: &[u32]) -> Option<u64> {
    #[cfg(unix)]
    {
        let pids = pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::process::Command::new("ps")
                .args(["-o", "rss=", "-p", &pids])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        if !result.status.success() {
            return None;
        }
        let values = std::str::from_utf8(&result.stdout)
            .ok()?
            .split_whitespace()
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        Some(values.iter().sum::<u64>() * 1024)
    }
    #[cfg(not(unix))]
    {
        let _ = pids;
        None
    }
}

async fn scheduler(host: Arc<Host>, app: tauri::AppHandle) {
    loop {
        host.wake.notified().await;
        if host.closing.load(Ordering::SeqCst) {
            return;
        }
        loop {
            let (run, rx) = match host.admit().await {
                Ok(Some(pair)) => pair,
                Ok(None) => break,
                Err(error) => {
                    let _ = app.emit("forge-error", error);
                    break;
                }
            };
            tauri::async_runtime::spawn(execute_run(host.clone(), app.clone(), run, rx));
        }
    }
}

async fn persist_text(
    host: &Arc<Host>,
    run_id: &str,
    pending: &mut Vec<(String, String)>,
) -> Result<(), String> {
    if pending.is_empty() {
        return Ok(());
    }
    let batch = std::mem::take(pending);
    let id = run_id.to_string();
    host.db(move |store| {
        for (role, text) in batch {
            store.append_output(&id, &role, &text)?;
        }
        Ok(())
    })
    .await
}

async fn cancel_after_storage_error(host: &Arc<Host>, run_id: &str, sent: &mut bool) {
    if *sent {
        return;
    }
    let sender = host
        .runtimes
        .lock()
        .ok()
        .and_then(|r| r.get(run_id).map(|r| r.control.clone()));
    if let Some(sender) = sender {
        *sent = sender.send(Control::Cancel).await.is_ok();
    }
}

async fn execute_run(
    host: Arc<Host>,
    app: tauri::AppHandle,
    run: Run,
    rx: mpsc::Receiver<Control>,
) {
    let id = run.id.clone();
    let result = async {
        let check_id = id.clone();
        let current = host.db(move |s| s.run(&check_id)).await?;
        if current.status == "cancelling" || host.closing.load(Ordering::SeqCst) {
            return Ok(forge_grok::RunOutcome {
                status: "cancelled".into(),
                error: None,
                usage: Default::default(),
            });
        }
        let settings = host.db(|s| s.settings()).await?;
        let provider = forge_grok::probe(&settings.grok_path).await;
        let executable = provider
            .executable
            .filter(|_| provider.available)
            .ok_or(provider.message)?;
        let (events_tx, mut events_rx) = mpsc::channel(128);
        let config = GrokRunConfig {
            executable: PathBuf::from(executable),
            workspace: PathBuf::from(&run.workspace),
            prompt: run.prompt,
            session_id: run.session_id,
        };
        let task = tauri::async_runtime::spawn(forge_grok::run(config, rx, events_tx));
        let mut persistence_error = None;
        let mut storage_cancel_sent = false;
        let mut pending: Vec<(String, String)> = Vec::new();
        let mut pending_bytes = 0usize;
        let mut flush_at = tokio::time::Instant::now();
        loop {
            let next = if pending.is_empty() {
                Some(events_rx.recv().await)
            } else {
                tokio::time::timeout_at(flush_at, events_rx.recv())
                    .await
                    .ok()
            };
            if !matches!(next, Some(Some(Event::Text { .. }))) {
                if let Err(error) = persist_text(&host, &id, &mut pending).await {
                    persistence_error = Some(error);
                }
                pending_bytes = 0;
            }
            if persistence_error.is_some() {
                cancel_after_storage_error(&host, &id, &mut storage_cancel_sent).await;
            }
            let event = match next {
                Some(Some(event)) => event,
                Some(None) => break,
                None => continue,
            };
            let event_id = id.clone();
            let saved = match event {
                Event::Started { pid } => {
                    if let Some(runtime) = host
                        .runtimes
                        .lock()
                        .map_err(|_| "Run controls are unavailable.")?
                        .get_mut(&id)
                    {
                        runtime.pid = Some(pid);
                    }
                    Ok(())
                }
                Event::Session(session) => {
                    host.db(move |s| s.set_session(&event_id, &session)).await
                }
                Event::Text { role, delta } => {
                    if pending.is_empty() {
                        flush_at =
                            tokio::time::Instant::now() + std::time::Duration::from_millis(75);
                    }
                    pending_bytes += delta.len();
                    if let Some((_, text)) = pending
                        .last_mut()
                        .filter(|(last_role, _)| *last_role == role)
                    {
                        text.push_str(&delta);
                    } else {
                        pending.push((role, delta));
                    }
                    if pending_bytes >= 8192 {
                        pending_bytes = 0;
                        persist_text(&host, &id, &mut pending).await
                    } else {
                        Ok(())
                    }
                }
                Event::Permission(mut question) => {
                    question.run_id = id.clone();
                    if let Some(runtime) = host
                        .runtimes
                        .lock()
                        .map_err(|_| "Run controls are unavailable.")?
                        .get_mut(&id)
                    {
                        runtime
                            .questions
                            .insert(question.request_id.clone(), question);
                    }
                    Ok(())
                }
                Event::Usage(_) => Ok(()),
            };
            if let Err(error) = saved {
                persistence_error = Some(error);
            }
            if persistence_error.is_some() {
                cancel_after_storage_error(&host, &id, &mut storage_cancel_sent).await;
            }
            let _ = app.emit("forge-changed", &id);
        }
        let outcome = task.await.map_err(|e| e.to_string())??;
        if let Some(error) = persistence_error {
            return Err(format!(
                "Output could not be saved; review Grok before retrying: {error}"
            ));
        }
        Ok::<_, String>(outcome)
    }
    .await;
    let finish = host.save_outcome(&id, result).await;
    if let Err(error) = finish {
        let _ = app.emit("forge-error", error);
    }
    host.runtimes.lock().expect("runtime lock").remove(&id);
    host.finished.notify_waiters();
    let _ = app.emit("forge-changed", &id);
    host.wake.notify_one();
}

fn main() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let data = std::env::var_os("FORGE_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            std::fs::create_dir_all(&data)?;
            let lock = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(data.join("instance.lock"))?;
            lock.try_lock_exclusive()
                .map_err(|_| "Forge V2 is already using this data folder.")?;
            let store = Store::open(&data.join("forge.sqlite3")).map_err(std::io::Error::other)?;
            let host = Arc::new(Host {
                store: Mutex::new(store),
                runtimes: Mutex::new(HashMap::new()),
                storage_faults: Mutex::new(HashMap::new()),
                wake: Notify::new(),
                finished: Notify::new(),
                closing: AtomicBool::new(false),
                exiting: AtomicBool::new(false),
                admission: tokio::sync::Mutex::new(()),
                _instance_lock: lock,
            });
            app.manage(host.clone());
            tauri::async_runtime::spawn(scheduler(host.clone(), app.handle().clone()));
            host.wake.notify_one();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![dispatch])
        .build(tauri::generate_context!())
        .expect("Forge V2 could not open its local data. Check that it is not already running.");
    app.run(|handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let host = handle.state::<Arc<Host>>().inner().clone();
            if host.exiting.load(Ordering::SeqCst) {
                return;
            }
            api.prevent_exit();
            if !host.closing.swap(true, Ordering::SeqCst) {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let admission = host.admission.lock().await;
                    let controls: Vec<_> = host
                        .runtimes
                        .lock()
                        .expect("runtime lock")
                        .values()
                        .map(|r| r.control.clone())
                        .collect();
                    for control in controls {
                        let _ = control.send(Control::Cancel).await;
                    }
                    drop(admission);
                    loop {
                        let finished = host.finished.notified();
                        if host.runtimes.lock().expect("runtime lock").is_empty() {
                            break;
                        }
                        finished.await;
                    }
                    host.exiting.store(true, Ordering::SeqCst);
                    handle.exit(0);
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_local_main_window_can_dispatch() {
        assert!(trusted_origin("main", "tauri://localhost"));
        assert!(trusted_origin("main", "https://tauri.localhost"));
        assert!(!trusted_origin("preview", "tauri://localhost"));
        assert!(!trusted_origin("main", "https://example.com"));
        assert!(!trusted_origin("main", "http://localhost:1427"));
    }

    fn fixture() -> (tempfile::TempDir, Arc<Host>, String) {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("test.sqlite3")).unwrap();
        let saved_id = |reply| match reply {
            Reply::Saved { id } => id,
            _ => panic!("expected saved ID"),
        };
        let project = saved_id(
            store
                .execute(Request::AddProject {
                    name: "Fixture".into(),
                    path: temp.path().to_string_lossy().into_owned(),
                })
                .unwrap(),
        );
        let thread = saved_id(
            store
                .execute(Request::CreateThread {
                    project_id: project,
                    title: "Test".into(),
                })
                .unwrap(),
        );
        let run_id = saved_id(
            store
                .execute(Request::QueueRun {
                    command_id: "race".into(),
                    thread_id: thread,
                    prompt: "Fixture only".into(),
                })
                .unwrap(),
        );
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(temp.path().join("lock"))
            .unwrap();
        let host = Arc::new(Host {
            store: Mutex::new(store),
            runtimes: Mutex::new(HashMap::new()),
            storage_faults: Mutex::new(HashMap::new()),
            wake: Notify::new(),
            finished: Notify::new(),
            closing: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            admission: tokio::sync::Mutex::new(()),
            _instance_lock: lock,
        });
        (temp, host, run_id)
    }

    #[tokio::test]
    async fn cancellation_racing_admission_always_reaches_the_owned_receiver() {
        let (_temp, host, run_id) = fixture();
        let (admitted, cancelled) = tokio::join!(host.admit(), host.cancel(run_id.clone()));
        cancelled.unwrap();
        if let Some((run, mut receiver)) = admitted.unwrap() {
            assert_eq!(run.id, run_id);
            assert!(matches!(receiver.try_recv().unwrap(), Control::Cancel));
        }
        let state = host.db(move |s| s.run(&run_id)).await.unwrap();
        assert!(["cancelled", "cancelling"].contains(&state.status.as_str()));
        host.closing.store(true, Ordering::SeqCst);
        assert!(host.admit().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn failed_final_save_is_visible_as_unknown_and_keeps_the_workspace_reserved() {
        let (temp, host, run_id) = fixture();
        let (_, mut controls) = host.admit().await.unwrap().unwrap();
        let conn = rusqlite::Connection::open(temp.path().join("test.sqlite3")).unwrap();
        conn.execute_batch("CREATE TRIGGER fail_final BEFORE UPDATE OF status ON runs BEGIN SELECT RAISE(FAIL, 'disk full fixture'); END;").unwrap();
        let mut stop_sent = false;
        cancel_after_storage_error(&host, &run_id, &mut stop_sent).await;
        cancel_after_storage_error(&host, &run_id, &mut stop_sent).await;
        assert!(matches!(controls.try_recv().unwrap(), Control::Cancel));
        assert!(controls.try_recv().is_err());
        assert!(
            host.save_outcome(
                &run_id,
                Ok(forge_grok::RunOutcome {
                    status: "completed".into(),
                    error: None,
                    usage: Default::default()
                })
            )
            .await
            .is_err()
        );
        let mut snapshot = host.db(|s| s.snapshot(None, None, None)).await.unwrap();
        host.apply_storage_faults(&mut snapshot).unwrap();
        let run = snapshot.runs.iter().find(|r| r.id == run_id).unwrap();
        assert_eq!(run.status, "unknown");
        assert!(run.error.as_ref().unwrap().contains("disk full fixture"));
        assert_eq!(host.db(|s| s.run_counts()).await.unwrap().0, 1);
        let leases: i64 = conn
            .query_row("SELECT count(*) FROM leases", [], |r| r.get(0))
            .unwrap();
        assert_eq!(leases, 1);
    }
}
