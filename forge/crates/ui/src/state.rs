use forge_domain::*;
use leptos::prelude::*;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["window", "__TAURI__", "core"], js_name = invoke)]
    async fn invoke(command: &str, args: JsValue) -> Result<JsValue, JsValue>;
}

#[derive(Serialize)]
struct Arguments {
    request: Request,
}

pub async fn dispatch(request: Request) -> Result<Reply, String> {
    let args = Arguments { request }
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| e.to_string())?;
    let result = invoke("dispatch", args).await.map_err(|e| {
        e.as_string().unwrap_or_else(|| {
            "The desktop connection is unavailable. Open Forge V2 as a desktop app.".into()
        })
    })?;
    serde_wasm_bindgen::from_value(result)
        .map_err(|e| format!("Could not read the app response: {e}"))
}

#[derive(Clone, Copy)]
pub struct Ui {
    pub snapshot: RwSignal<Snapshot>,
    pub selected: RwSignal<Option<String>>,
    pub draft: RwSignal<String>,
    pub dirty: RwSignal<bool>,
    pub draft_revision: RwSignal<u64>,
    pub loading: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    pub error: RwSignal<String>,
    pub notice: RwSignal<String>,
    pub panel: RwSignal<String>,
    pub note_anchor: RwSignal<Option<String>>,
    pub before: RwSignal<Option<i64>>,
    pub search: RwSignal<String>,
    pub provider: RwSignal<Option<ProviderStatus>>,
    pub polling: RwSignal<bool>,
    pub workspace: RwSignal<Option<WorkspaceView>>,
    pub directory: RwSignal<String>,
    pub file: RwSignal<Option<(String, String, bool)>>,
    pub resources: RwSignal<Option<ResourceReport>>,
    pub pending_send: RwSignal<Option<(String, String, String)>>,
    draft_gate: StoredValue<Arc<futures::lock::Mutex<()>>>,
    local_drafts: RwSignal<BTreeMap<String, String>>,
    pub output: RwSignal<Option<OutputPage>>,
    pub output_history: RwSignal<Vec<u64>>,
    requested_file: RwSignal<Option<(String, String)>>,
    requested_output: RwSignal<Option<(String, String, u64)>>,
}

#[derive(Clone)]
pub struct OutputPage {
    pub run_id: String,
    pub role: String,
    pub offset: u64,
    pub text: String,
    pub next_offset: Option<u64>,
}

impl Ui {
    pub fn new() -> Self {
        Self {
            snapshot: RwSignal::new(Snapshot::default()),
            selected: RwSignal::new(None),
            draft: RwSignal::new(String::new()),
            dirty: RwSignal::new(false),
            draft_revision: RwSignal::new(0),
            loading: RwSignal::new(true),
            busy: RwSignal::new(false),
            error: RwSignal::new(String::new()),
            notice: RwSignal::new(String::new()),
            panel: RwSignal::new(String::new()),
            note_anchor: RwSignal::new(None),
            before: RwSignal::new(None),
            search: RwSignal::new(String::new()),
            provider: RwSignal::new(None),
            polling: RwSignal::new(false),
            workspace: RwSignal::new(None),
            directory: RwSignal::new(String::new()),
            file: RwSignal::new(None),
            resources: RwSignal::new(None),
            pending_send: RwSignal::new(None),
            draft_gate: StoredValue::new(Arc::new(futures::lock::Mutex::new(()))),
            local_drafts: RwSignal::new(BTreeMap::new()),
            output: RwSignal::new(None),
            output_history: RwSignal::new(Vec::new()),
            requested_file: RwSignal::new(None),
            requested_output: RwSignal::new(None),
        }
    }

    pub fn thread(self) -> Option<Thread> {
        let selected = self.selected.get();
        self.snapshot.with(|s| {
            s.threads
                .iter()
                .find(|t| Some(&t.id) == selected.as_ref())
                .cloned()
        })
    }

    pub fn project(self) -> Option<Project> {
        let id = self.thread().map(|t| t.project_id);
        self.snapshot.with(|s| {
            s.projects
                .iter()
                .find(|p| Some(&p.id) == id.as_ref())
                .cloned()
        })
    }

    pub fn layout(self) -> String {
        self.thread()
            .map(|t| t.layout)
            .unwrap_or_else(|| "focus".into())
    }

    pub fn refresh(self, reset_draft: bool) {
        leptos::task::spawn_local(async move {
            self.load(reset_draft).await;
        });
    }

    pub async fn load(self, reset_draft: bool) {
        let selected = self.selected.get_untracked();
        let before = self.before.get_untracked();
        let search = self.search.get_untracked();
        let revision = self.draft_revision.get_untracked();
        match dispatch(Request::Snapshot {
            thread_id: selected.clone(),
            before,
            search: (!search.is_empty()).then_some(search.clone()),
        })
        .await
        {
            Ok(Reply::Snapshot(mut snapshot))
                if self.selected.get_untracked() == selected
                    && self.before.get_untracked() == before
                    && self.search.get_untracked() == search =>
            {
                self.local_drafts.with_untracked(|drafts| {
                    for thread in &mut snapshot.threads {
                        if let Some(draft) = drafts.get(&thread.id) {
                            thread.draft = draft.clone();
                        }
                    }
                });
                let current = selected.or_else(|| snapshot.selected_thread.clone());
                if reset_draft
                    && !self.dirty.get_untracked()
                    && self.draft_revision.get_untracked() == revision
                {
                    self.draft.set(
                        snapshot
                            .threads
                            .iter()
                            .find(|t| Some(&t.id) == current.as_ref())
                            .map(|t| t.draft.clone())
                            .unwrap_or_default(),
                    );
                }
                self.selected.set(current);
                self.snapshot.set(snapshot);
            }
            Ok(_) => {}
            Err(error) => self.error.set(error),
        }
        self.loading.set(false);
    }

    pub fn mutate(self, request: Request, notice: &'static str) {
        self.error.set(String::new());
        leptos::task::spawn_local(async move {
            match dispatch(request).await {
                Ok(_) => {
                    if !notice.is_empty() {
                        self.notice.set(notice.into());
                    }
                    self.load(false).await;
                }
                Err(error) => self.error.set(error),
            }
        });
    }

    pub fn select_thread(self, id: String) {
        self.save_draft_now();
        self.draft_revision.update(|v| *v += 1);
        self.dirty.set(false);
        self.draft.set(
            self.local_drafts
                .with_untracked(|drafts| drafts.get(&id).cloned())
                .unwrap_or_else(|| {
                    self.snapshot.with_untracked(|s| {
                        s.threads
                            .iter()
                            .find(|t| t.id == id)
                            .map(|t| t.draft.clone())
                            .unwrap_or_default()
                    })
                }),
        );
        self.selected.set(Some(id));
        self.before.set(None);
        self.search.set(String::new());
        self.workspace.set(None);
        self.directory.set(String::new());
        self.file.set(None);
        self.note_anchor.set(None);
        self.panel.set(String::new());
        self.refresh(true);
    }

    pub fn new_thread(self, project_id: String) {
        self.save_draft_now();
        leptos::task::spawn_local(async move {
            match dispatch(Request::CreateThread {
                project_id,
                title: String::new(),
            })
            .await
            {
                Ok(Reply::Saved { id }) => self.select_thread(id),
                Ok(_) => self.refresh(true),
                Err(error) => self.error.set(error),
            }
        });
    }

    pub fn edit_draft(self, draft: String) {
        self.draft.set(draft.clone());
        if let Some(thread_id) = self.selected.get_untracked() {
            self.local_drafts.update(|drafts| {
                drafts.insert(thread_id, draft.clone());
            });
        }
        self.dirty.set(true);
        self.draft_revision.update(|v| *v += 1);
        let revision = self.draft_revision.get_untracked();
        let thread_id = self.selected.get_untracked();
        let gate = self.draft_gate.get_value();
        leptos::task::spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(300).await;
            if self.draft_revision.get_untracked() != revision {
                return;
            }
            let _guard = gate.lock().await;
            if self.draft_revision.get_untracked() != revision {
                return;
            }
            if let Some(thread_id) = thread_id {
                match dispatch(Request::SaveDraft { thread_id, draft }).await {
                    Ok(_) if self.draft_revision.get_untracked() == revision => {
                        self.dirty.set(false)
                    }
                    Err(error) => self.error.set(format!("Draft has not been saved: {error}")),
                    _ => {}
                }
            }
        });
    }

    pub fn save_draft_now(self) {
        if !self.dirty.get_untracked() {
            return;
        }
        if let Some(thread_id) = self.selected.get_untracked() {
            let draft = self.draft.get_untracked();
            if self.busy.get_untracked()
                && self.pending_send.with_untracked(|p| {
                    p.as_ref()
                        .is_some_and(|(_, t, text)| t == &thread_id && text == draft.trim())
                })
            {
                return;
            }
            let gate = self.draft_gate.get_value();
            leptos::task::spawn_local(async move {
                let _guard = gate.lock().await;
                if let Err(error) = dispatch(Request::SaveDraft { thread_id, draft }).await {
                    self.error.set(format!("Draft has not been saved: {error}"));
                }
            });
        }
    }

    pub fn send(self) {
        if self.busy.get_untracked() {
            return;
        }
        let Some(thread_id) = self.selected.get_untracked() else {
            return;
        };
        let prompt = self.draft.get_untracked().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let command_id = self
            .pending_send
            .with_untracked(|pending| {
                pending
                    .as_ref()
                    .filter(|(_, t, p)| *t == thread_id && *p == prompt)
                    .map(|(id, _, _)| id.clone())
            })
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.pending_send.set(Some((
            command_id.clone(),
            thread_id.clone(),
            prompt.clone(),
        )));
        self.busy.set(true);
        self.error.set(String::new());
        self.draft_revision.update(|v| *v += 1);
        let revision = self.draft_revision.get_untracked();
        let gate = self.draft_gate.get_value();
        leptos::task::spawn_local(async move {
            let _guard = gate.lock().await;
            if let Err(error) = dispatch(Request::SaveDraft {
                thread_id: thread_id.clone(),
                draft: prompt.clone(),
            })
            .await
            {
                self.error.set(format!("Draft could not be saved: {error}"));
                self.busy.set(false);
                return;
            }
            match dispatch(Request::QueueRun {
                command_id,
                thread_id: thread_id.clone(),
                prompt: prompt.clone(),
            })
            .await
            {
                Ok(_) => {
                    self.pending_send.set(None);
                    self.local_drafts.update(|drafts| {
                        if drafts
                            .get(&thread_id)
                            .is_some_and(|draft| draft.trim() == prompt)
                        {
                            drafts.insert(thread_id.clone(), String::new());
                        }
                    });
                    if self.selected.get_untracked().as_ref() == Some(&thread_id)
                        && self.draft_revision.get_untracked() == revision
                    {
                        self.draft.set(String::new());
                        self.dirty.set(false);
                        self.before.set(None);
                        self.search.set(String::new());
                    }
                    self.load(false).await;
                }
                Err(error) => self.error.set(error),
            }
            self.busy.set(false);
        });
    }

    pub fn open_panel(self, name: &str) {
        self.panel.set(name.into());
    }

    pub fn toggle_theme(self) {
        let mut settings = self.snapshot.with_untracked(|s| s.settings.clone());
        settings.theme = if settings.theme == "carbon" {
            "paper"
        } else {
            "carbon"
        }
        .into();
        self.mutate(Request::SaveSettings { settings }, "");
    }

    pub fn set_layout(self, layout: &str) {
        if let Some(thread_id) = self.selected.get_untracked() {
            self.mutate(
                Request::SetLayout {
                    thread_id,
                    layout: layout.into(),
                },
                "",
            );
            if layout != "focus" {
                self.load_workspace(String::new());
            }
        }
    }

    pub fn check_provider(self) {
        leptos::task::spawn_local(async move {
            match dispatch(Request::ProviderStatus).await {
                Ok(Reply::ProviderStatus(status)) => self.provider.set(Some(status)),
                Err(error) => self.error.set(error),
                _ => {}
            }
        });
    }

    pub fn load_workspace(self, directory: String) {
        if let Some(project) = self.project() {
            self.directory.set(directory.clone());
            self.file.set(None);
            self.requested_file.set(None);
            let project_id = project.id;
            leptos::task::spawn_local(async move {
                match dispatch(Request::Workspace {
                    project_id: project_id.clone(),
                    directory: directory.clone(),
                })
                .await
                {
                    Ok(Reply::Workspace(workspace))
                        if self.project().as_ref().is_some_and(|p| p.id == project_id)
                            && self.directory.get_untracked() == directory =>
                    {
                        self.workspace.set(Some(workspace))
                    }
                    Err(error) => self.error.set(error),
                    _ => {}
                }
            });
        }
    }

    pub fn read_file(self, path: String) {
        if let Some(project) = self.project() {
            let request = (project.id.clone(), path.clone());
            self.requested_file.set(Some(request.clone()));
            leptos::task::spawn_local(async move {
                match dispatch(Request::ReadFile {
                    project_id: project.id,
                    path,
                })
                .await
                {
                    Ok(Reply::File {
                        path,
                        text,
                        truncated,
                    }) if self.requested_file.get_untracked().as_ref() == Some(&request)
                        && self.project().as_ref().is_some_and(|p| p.id == request.0) =>
                    {
                        self.file.set(Some((path, text, truncated)))
                    }
                    Err(error) => self.error.set(error),
                    _ => {}
                }
            });
        }
    }

    pub fn measure_resources(self) {
        leptos::task::spawn_local(async move {
            match dispatch(Request::Resources).await {
                Ok(Reply::Resources(report)) => self.resources.set(Some(report)),
                Err(error) => self.error.set(error),
                _ => {}
            }
        });
    }

    pub fn read_output(self, run_id: String, role: String, offset: u64) {
        self.open_panel("output");
        let request = (run_id.clone(), role.clone(), offset);
        self.requested_output.set(Some(request.clone()));
        leptos::task::spawn_local(async move {
            match dispatch(Request::ReadOutput {
                run_id,
                role,
                offset,
            })
            .await
            {
                Ok(Reply::Output {
                    run_id,
                    role,
                    offset,
                    text,
                    next_offset,
                }) if self.requested_output.get_untracked().as_ref() == Some(&request) => {
                    self.output.set(Some(OutputPage {
                        run_id,
                        role,
                        offset,
                        text,
                        next_offset,
                    }))
                }
                Err(error) => self.error.set(error),
                _ => {}
            }
        });
    }
}

pub fn is_active(status: &str) -> bool {
    matches!(
        status,
        "queued" | "starting" | "running" | "waiting" | "cancelling"
    )
}
pub fn title_case(text: &str) -> String {
    let mut c = text.chars();
    c.next()
        .map(|first| first.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}
pub fn run_label(status: &str) -> String {
    match status {
        "unknown" => "NEEDS RECONCILIATION".into(),
        "queued" => "QUEUED · WAITING FOR A FREE WORKSPACE".into(),
        "running" => "GROK IS WORKING".into(),
        _ => status.to_uppercase(),
    }
}
pub fn bytes_label(value: Option<u64>) -> String {
    value
        .map(|v| format!("{:.1} MiB", v as f64 / 1_048_576.0))
        .unwrap_or_else(|| "Not reported".into())
}
