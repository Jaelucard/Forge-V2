use serde::{Deserialize, Serialize};

pub const HISTORY_PAGE: usize = 80;
pub const MAX_ACTIVE_RUNS: usize = 4;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub brief: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Thread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub draft: String,
    pub layout: String,
    pub session_id: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub sequence: i64,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub role: String,
    pub body: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub id: String,
    pub thread_id: String,
    pub message_id: Option<String>,
    pub body: String,
    pub pinned: bool,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PromptTemplate {
    pub id: String,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Run {
    pub id: String,
    pub command_id: String,
    pub thread_id: String,
    pub workspace: String,
    pub prompt: String,
    pub status: String,
    pub session_id: Option<String>,
    pub error: Option<String>,
    pub usage: Usage,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Activity {
    pub id: i64,
    pub thread_id: String,
    pub kind: String,
    pub summary: String,
    pub source_id: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub theme: String,
    pub grok_path: String,
    pub max_active_runs: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "paper".into(),
            grok_path: String::new(),
            max_active_runs: MAX_ACTIVE_RUNS,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub projects: Vec<Project>,
    pub threads: Vec<Thread>,
    pub selected_thread: Option<String>,
    pub messages: Vec<Message>,
    pub has_older_messages: bool,
    pub notes: Vec<Note>,
    pub templates: Vec<PromptTemplate>,
    pub runs: Vec<Run>,
    pub activities: Vec<Activity>,
    pub settings: Settings,
    pub questions: Vec<PermissionQuestion>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub kind: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PermissionQuestion {
    pub run_id: String,
    pub request_id: String,
    pub title: String,
    pub options: Vec<PermissionOption>,
    pub kind: String,
    pub questions: Vec<UserQuestion>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct UserQuestion {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
    pub multi_select: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderStatus {
    pub available: bool,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub message: String,
    pub auth_status: String,
    pub fan_out: String,
    pub workspace_authority: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceView {
    pub branch: Option<String>,
    pub status: String,
    pub diff: String,
    pub files: Vec<FileEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ResourceReport {
    pub app_rss_bytes: Option<u64>,
    pub provider_rss_bytes: Option<u64>,
    pub active_runs: usize,
    pub queued_runs: usize,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Snapshot {
        thread_id: Option<String>,
        before: Option<i64>,
        search: Option<String>,
    },
    AddProject {
        name: String,
        path: String,
    },
    SaveBrief {
        project_id: String,
        brief: String,
    },
    CreateThread {
        project_id: String,
        title: String,
    },
    SaveDraft {
        thread_id: String,
        draft: String,
    },
    SetLayout {
        thread_id: String,
        layout: String,
    },
    SaveNote {
        note: Note,
    },
    DeleteNote {
        id: String,
    },
    SaveTemplate {
        template: PromptTemplate,
    },
    DeleteTemplate {
        id: String,
    },
    SaveSettings {
        settings: Settings,
    },
    MarkDecision {
        thread_id: String,
        message_id: String,
    },
    QueueRun {
        command_id: String,
        thread_id: String,
        prompt: String,
    },
    CancelRun {
        run_id: String,
    },
    ResolveRun {
        run_id: String,
    },
    AnswerPermission {
        run_id: String,
        request_id: String,
        option_id: String,
    },
    AnswerUserInput {
        run_id: String,
        request_id: String,
        answers: std::collections::HashMap<String, Vec<String>>,
    },
    ProviderStatus,
    Workspace {
        project_id: String,
        directory: String,
    },
    ReadFile {
        project_id: String,
        path: String,
    },
    ReadOutput {
        run_id: String,
        role: String,
        offset: u64,
    },
    Resources,
    PickFolder,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Reply {
    Snapshot(Snapshot),
    Saved {
        id: String,
    },
    ProviderStatus(ProviderStatus),
    Workspace(WorkspaceView),
    File {
        path: String,
        text: String,
        truncated: bool,
    },
    Resources(ResourceReport),
    Folder(Option<String>),
    Output {
        run_id: String,
        role: String,
        offset: u64,
        text: String,
        next_offset: Option<u64>,
    },
}
