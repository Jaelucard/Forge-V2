use forge_domain::*;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, String>;
const BODY_LIMIT: usize = 256 * 1024;
const LOCAL_LIMIT: usize = 64 * 1024;
const OUTPUT_PAGE: usize = 64 * 1024;
const OUTPUT_NOTICE: &str = "\n[Preview shortened; full output saved.]";
const ACTIVE: &str = "'starting','running','waiting','cancelling','unknown'";

pub struct Store {
    conn: Connection,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn id() -> String {
    Uuid::new_v4().to_string()
}
fn bounded(s: &str, max: usize, label: &str) -> Result<()> {
    if s.len() > max {
        return Err(format!("{label} is too long (maximum {max} bytes)."));
    }
    if s.contains('\0') {
        return Err(format!("{label} contains an invalid character."));
    }
    Ok(())
}
fn required(s: &str, max: usize, label: &str) -> Result<()> {
    bounded(s, max, label)?;
    if s.trim().is_empty() {
        return Err(format!("{label} cannot be empty."));
    }
    Ok(())
}
fn changed(n: usize, label: &str) -> Result<()> {
    if n == 0 {
        Err(format!("{label} was not found."))
    } else {
        Ok(())
    }
}
fn project_row(r: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        path: r.get(2)?,
        brief: r.get(3)?,
        created_at: r.get(4)?,
    })
}
fn thread_row(r: &Row<'_>) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: r.get(0)?,
        project_id: r.get(1)?,
        title: r.get(2)?,
        draft: r.get(3)?,
        layout: r.get(4)?,
        session_id: r.get(5)?,
        created_at: r.get(6)?,
    })
}
fn message_row(r: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        sequence: r.get(1)?,
        thread_id: r.get(2)?,
        run_id: r.get(3)?,
        role: r.get(4)?,
        body: r.get(5)?,
        created_at: r.get(6)?,
    })
}
fn run_row(r: &Row<'_>) -> rusqlite::Result<Run> {
    let usage: String = r.get(8)?;
    Ok(Run {
        id: r.get(0)?,
        command_id: r.get(1)?,
        thread_id: r.get(2)?,
        workspace: r.get(3)?,
        prompt: r.get(4)?,
        status: r.get(5)?,
        session_id: r.get(6)?,
        error: r.get(7)?,
        usage: serde_json::from_str(&usage).unwrap_or_default(),
        created_at: r.get(9)?,
    })
}
const PROJECT_COLS: &str = "id,name,path,brief,created_at";
const THREAD_COLS: &str = "id,project_id,title,draft,layout,session_id,created_at";
const MESSAGE_COLS: &str = "id,sequence,thread_id,run_id,role,body,created_at";
const RUN_COLS: &str =
    "id,command_id,thread_id,workspace,prompt,status,session_id,error,usage,created_at";

fn record(
    tx: &Transaction<'_>,
    thread: &str,
    kind: &str,
    summary: &str,
    source: &str,
) -> Result<()> {
    tx.execute("INSERT INTO activities(thread_id,kind,summary,source_id,created_at) VALUES(?1,?2,?3,?4,?5)",params![thread,kind,summary,source,now()]).map_err(err)?;
    Ok(())
}

fn canonical_workspace(path: &Path) -> Result<(String, String)> {
    if !path.is_absolute() {
        return Err("Choose an absolute workspace folder.".into());
    }
    let real = path
        .canonicalize()
        .map_err(|_| "Workspace folder is unavailable.".to_string())?;
    if !real.is_dir() {
        return Err("The workspace must be a folder.".into());
    }
    let checkout = real
        .ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap_or(&real);
    let real_text = real
        .to_str()
        .ok_or("Workspace path is not valid Unicode.")?
        .to_owned();
    let key = checkout
        .to_str()
        .ok_or("Checkout path is not valid Unicode.")?
        .to_owned();
    Ok((real_text, key))
}

fn save_chunks(tx: &Transaction<'_>, run: &str, role: &str, mut text: &str) -> Result<()> {
    let mut offset: i64 = tx.query_row(
        "SELECT COALESCE((SELECT offset+byte_length FROM output_chunks WHERE run_id=?1 AND role=?2 ORDER BY offset DESC LIMIT 1),0)",
        params![run, role], |r| r.get(0),
    ).map_err(err)?;
    while !text.is_empty() {
        let mut end = text.len().min(OUTPUT_PAGE);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let next = offset
            .checked_add(end as i64)
            .ok_or("Saved output exceeds the supported size.")?;
        tx.execute(
            "INSERT INTO output_chunks(run_id,role,offset,body,byte_length) VALUES(?1,?2,?3,?4,?5)",
            params![run, role, offset, &text[..end], end as i64],
        )
        .map_err(err)?;
        offset = next;
        text = &text[end..];
    }
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(err)?;
        }
        let mut conn = Connection::open(path).map_err(err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )
        .map_err(err)?;
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(err)?;
        if version > 2 {
            return Err(
                "This history was created by a newer Forge version. Open it with that version."
                    .into(),
            );
        }
        if version == 0 {
            let tables:i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0)).map_err(err)?;
            if tables != 0 {
                return Err(
                    "This file is not an empty Forge database. Existing data has not been changed."
                        .into(),
                );
            }
            conn.execute_batch(include_str!("schema.sql"))
                .map_err(err)?;
        }
        if version == 1 {
            let backup = path.with_extension(format!("pre-v2-{}.sqlite", id()));
            conn.execute(
                "VACUUM INTO ?1",
                [backup
                    .to_str()
                    .ok_or("History backup path is not valid Unicode.")?],
            )
            .map_err(err)?;
            std::fs::File::open(&backup)
                .and_then(|f| f.sync_all())
                .map_err(err)?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(err)?;
            tx.execute_batch(include_str!("output-schema.sql"))
                .map_err(err)?;
            {
                let mut statement = tx.prepare("SELECT run_id,role,body FROM messages WHERE run_id IS NOT NULL AND role IN ('assistant','tool')").map_err(err)?;
                let mut rows = statement.query([]).map_err(err)?;
                while let Some(row) = rows.next().map_err(err)? {
                    let run: String = row.get(0).map_err(err)?;
                    let role: String = row.get(1).map_err(err)?;
                    let body: String = row.get(2).map_err(err)?;
                    save_chunks(&tx, &run, &role, &body)?;
                }
            }
            tx.pragma_update(None, "user_version", 2).map_err(err)?;
            tx.commit().map_err(err)?;
        }
        let mut store = Self { conn };
        store.recover()?;
        Ok(store)
    }

    fn recover(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let active: Vec<(String, String)> = {
            let mut stmt=tx.prepare("SELECT id,thread_id FROM runs WHERE status IN ('starting','running','waiting','cancelling')").map_err(err)?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(err)?
                .collect::<rusqlite::Result<_>>()
                .map_err(err)?
        };
        for (run, thread) in active {
            tx.execute("UPDATE runs SET status='unknown',error='Forge restarted before the provider outcome was confirmed. Check the provider before releasing this workspace.' WHERE id=?1",[&run]).map_err(err)?;
            record(
                &tx,
                &thread,
                "recovery",
                "Run outcome is unknown after restart; workspace remains reserved.",
                &run,
            )?;
        }
        tx.commit().map_err(err)
    }

    pub fn settings(&self) -> Result<Settings> {
        let data: String = self
            .conn
            .query_row("SELECT value FROM settings WHERE key='app'", [], |r| {
                r.get(0)
            })
            .map_err(err)?;
        serde_json::from_str(&data).map_err(err)
    }
    pub fn project(&self, project_id: &str) -> Result<Project> {
        self.conn
            .query_row(
                &format!("SELECT {PROJECT_COLS} FROM projects WHERE id=?1"),
                [project_id],
                project_row,
            )
            .optional()
            .map_err(err)?
            .ok_or("Project was not found.".into())
    }
    pub fn run(&self, run_id: &str) -> Result<Run> {
        self.conn
            .query_row(
                &format!("SELECT {RUN_COLS} FROM runs WHERE id=?1"),
                [run_id],
                run_row,
            )
            .optional()
            .map_err(err)?
            .ok_or("Run was not found.".into())
    }
    pub fn run_counts(&self) -> Result<(usize, usize)> {
        let active: i64 = self
            .conn
            .query_row(
                &format!("SELECT count(*) FROM runs WHERE status IN ({ACTIVE})"),
                [],
                |r| r.get(0),
            )
            .map_err(err)?;
        let queued: i64 = self
            .conn
            .query_row("SELECT count(*) FROM runs WHERE status='queued'", [], |r| {
                r.get(0)
            })
            .map_err(err)?;
        Ok((active as usize, queued as usize))
    }

    pub fn snapshot(
        &self,
        thread: Option<&str>,
        before: Option<i64>,
        search: Option<&str>,
    ) -> Result<Snapshot> {
        let search = search.unwrap_or("").trim();
        bounded(search, 256, "Search")?;
        let like = format!(
            "%{}%",
            search
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut projects: Vec<Project> = self
            .conn
            .prepare(&format!(
                "SELECT {PROJECT_COLS} FROM projects ORDER BY created_at DESC,id LIMIT 500"
            ))
            .map_err(err)?
            .query_map([], project_row)
            .map_err(err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(err)?;
        let mut threads: Vec<Thread> = self
            .conn
            .prepare(&format!(
                "SELECT {THREAD_COLS} FROM threads ORDER BY created_at DESC,id LIMIT 500"
            ))
            .map_err(err)?
            .query_map([], thread_row)
            .map_err(err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(err)?;
        let selected = match thread {
            Some(t) => {
                let exists: bool = self
                    .conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1)",
                        [t],
                        |r| r.get(0),
                    )
                    .map_err(err)?;
                if !exists {
                    return Err("Conversation was not found.".into());
                }
                Some(t.to_string())
            }
            None => threads.first().map(|t| t.id.clone()),
        };
        if let Some(selected_id) = &selected {
            if !threads.iter().any(|t| &t.id == selected_id) {
                let selected_thread = self
                    .conn
                    .query_row(
                        &format!("SELECT {THREAD_COLS} FROM threads WHERE id=?1"),
                        [selected_id],
                        thread_row,
                    )
                    .map_err(err)?;
                threads.push(selected_thread);
            }
            let project_id = &threads
                .iter()
                .find(|t| &t.id == selected_id)
                .ok_or("Conversation was not found.")?
                .project_id;
            if !projects.iter().any(|p| &p.id == project_id) {
                projects.push(self.project(project_id)?);
            }
        }
        let mut snapshot = Snapshot {
            projects,
            threads,
            selected_thread: selected.clone(),
            settings: self.settings()?,
            ..Default::default()
        };
        if let Some(thread) = selected {
            let mut messages = Vec::with_capacity(HISTORY_PAGE + 1);
            let limit = if search.is_empty() {
                HISTORY_PAGE as i64 + 1
            } else {
                -1
            };
            let needle = search.to_ascii_lowercase();
            let mut statement = self.conn.prepare(&format!("SELECT {MESSAGE_COLS},body LIKE ?4 ESCAPE '\\' FROM messages WHERE thread_id=?1 AND (?2 IS NULL OR sequence<?2) ORDER BY sequence DESC LIMIT ?3")).map_err(err)?;
            let mut rows = statement
                .query(params![thread, before, limit, like])
                .map_err(err)?;
            while let Some(row) = rows.next().map_err(err)? {
                let message = message_row(row).map_err(err)?;
                let preview_matches: bool = row.get(7).map_err(err)?;
                let matches = if preview_matches {
                    true
                } else if let Some(run) = &message.run_id {
                    self.output_contains(run, &message.role, &needle)?
                } else {
                    false
                };
                if matches {
                    messages.push(message);
                }
                if messages.len() > HISTORY_PAGE {
                    break;
                }
            }
            snapshot.has_older_messages = messages.len() > HISTORY_PAGE;
            messages.truncate(HISTORY_PAGE);
            messages.reverse();
            snapshot.messages = messages;
            snapshot.notes=self.conn.prepare("SELECT id,thread_id,message_id,body,pinned,created_at FROM notes WHERE thread_id=?1 ORDER BY pinned DESC,created_at DESC,id LIMIT 500").map_err(err)?.query_map([&thread],|r|Ok(Note{id:r.get(0)?,thread_id:r.get(1)?,message_id:r.get(2)?,body:r.get(3)?,pinned:r.get(4)?,created_at:r.get(5)?})).map_err(err)?.collect::<rusqlite::Result<_>>().map_err(err)?;
            snapshot.activities=self.conn.prepare("SELECT id,thread_id,kind,summary,source_id,created_at FROM activities WHERE thread_id=?1 ORDER BY id DESC LIMIT 100").map_err(err)?.query_map([&thread],|r|Ok(Activity{id:r.get(0)?,thread_id:r.get(1)?,kind:r.get(2)?,summary:r.get(3)?,source_id:r.get(4)?,created_at:r.get(5)?})).map_err(err)?.collect::<rusqlite::Result<_>>().map_err(err)?;
        }
        for query in [
            format!(
                "SELECT {RUN_COLS} FROM runs WHERE status IN ({ACTIVE}) ORDER BY created_at,id"
            ),
            format!(
                "SELECT {RUN_COLS} FROM runs WHERE status='queued' ORDER BY created_at,rowid LIMIT 100"
            ),
            format!(
                "SELECT {RUN_COLS} FROM runs WHERE status IN ('completed','cancelled','failed','interrupted') ORDER BY created_at DESC,id LIMIT 100"
            ),
        ] {
            let mut statement = self.conn.prepare(&query).map_err(err)?;
            let runs = statement
                .query_map([], run_row)
                .map_err(err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(err)?;
            snapshot.runs.extend(runs);
        }
        snapshot.templates = self
            .conn
            .prepare("SELECT id,title,body FROM templates ORDER BY title,id LIMIT 500")
            .map_err(err)?
            .query_map([], |r| {
                Ok(PromptTemplate {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    body: r.get(2)?,
                })
            })
            .map_err(err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(err)?;
        Ok(snapshot)
    }

    pub fn execute(&mut self, request: Request) -> Result<Reply> {
        match request {
            Request::Snapshot {
                thread_id,
                before,
                search,
            } => self
                .snapshot(thread_id.as_deref(), before, search.as_deref())
                .map(Reply::Snapshot),
            Request::AddProject { name, path } => {
                required(&name, 200, "Project name")?;
                let (path, _) = canonical_workspace(Path::new(&path))?;
                let existing: Option<String> = self
                    .conn
                    .query_row("SELECT id FROM projects WHERE path=?1", [&path], |r| {
                        r.get(0)
                    })
                    .optional()
                    .map_err(err)?;
                if let Some(id) = existing {
                    return Ok(Reply::Saved { id });
                }
                let project = id();
                self.conn
                    .execute(
                        "INSERT INTO projects(id,name,path,created_at) VALUES(?1,?2,?3,?4)",
                        params![project, name.trim(), path, now()],
                    )
                    .map_err(err)?;
                Ok(Reply::Saved { id: project })
            }
            Request::SaveBrief { project_id, brief } => {
                bounded(&brief, LOCAL_LIMIT, "Project brief")?;
                changed(
                    self.conn
                        .execute(
                            "UPDATE projects SET brief=?1 WHERE id=?2",
                            params![brief, project_id],
                        )
                        .map_err(err)?,
                    "Project",
                )?;
                Ok(Reply::Saved { id: project_id })
            }
            Request::CreateThread { project_id, title } => {
                self.project(&project_id)?;
                bounded(&title, 200, "Conversation title")?;
                let title = if title.trim().is_empty() {
                    "New conversation"
                } else {
                    title.trim()
                };
                let thread = id();
                self.conn
                    .execute(
                        "INSERT INTO threads(id,project_id,title,created_at) VALUES(?1,?2,?3,?4)",
                        params![thread, project_id, title, now()],
                    )
                    .map_err(err)?;
                Ok(Reply::Saved { id: thread })
            }
            Request::SaveDraft { thread_id, draft } => {
                bounded(&draft, LOCAL_LIMIT, "Draft")?;
                changed(
                    self.conn
                        .execute(
                            "UPDATE threads SET draft=?1 WHERE id=?2",
                            params![draft, thread_id],
                        )
                        .map_err(err)?,
                    "Conversation",
                )?;
                Ok(Reply::Saved { id: thread_id })
            }
            Request::SetLayout { thread_id, layout } => {
                if !["focus", "review", "explore"].contains(&layout.as_str()) {
                    return Err("Choose Focus, Review or Explore.".into());
                }
                changed(
                    self.conn
                        .execute(
                            "UPDATE threads SET layout=?1 WHERE id=?2",
                            params![layout, thread_id],
                        )
                        .map_err(err)?,
                    "Conversation",
                )?;
                Ok(Reply::Saved { id: thread_id })
            }
            Request::SaveNote { mut note } => {
                required(&note.body, LOCAL_LIMIT, "Note")?;
                if note.id.is_empty() {
                    note.id = id();
                }
                let tx = self
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(err)?;
                if let Some(message) = &note.message_id {
                    let matches: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND thread_id=?2)",
                            params![message, note.thread_id],
                            |r| r.get(0),
                        )
                        .map_err(err)?;
                    if !matches {
                        return Err("The note's message is not in this conversation.".into());
                    }
                }
                let existing: Option<String> = tx
                    .query_row("SELECT thread_id FROM notes WHERE id=?1", [&note.id], |r| {
                        r.get(0)
                    })
                    .optional()
                    .map_err(err)?;
                if existing.as_ref().is_some_and(|x| x != &note.thread_id) {
                    return Err("A note cannot move between conversations.".into());
                }
                tx.execute("INSERT INTO notes(id,thread_id,message_id,body,pinned,created_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET message_id=excluded.message_id,body=excluded.body,pinned=excluded.pinned",params![note.id,note.thread_id,note.message_id,note.body,note.pinned,now()]).map_err(err)?;
                record(&tx, &note.thread_id, "note", "Margin note saved.", &note.id)?;
                tx.commit().map_err(err)?;
                Ok(Reply::Saved { id: note.id })
            }
            Request::DeleteNote { id } => {
                changed(
                    self.conn
                        .execute("DELETE FROM notes WHERE id=?1", [&id])
                        .map_err(err)?,
                    "Note",
                )?;
                Ok(Reply::Saved { id })
            }
            Request::SaveTemplate { mut template } => {
                required(&template.title, 200, "Template title")?;
                required(&template.body, LOCAL_LIMIT, "Template")?;
                if template.id.is_empty() {
                    template.id = id();
                }
                self.conn.execute("INSERT INTO templates(id,title,body) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET title=excluded.title,body=excluded.body",params![template.id,template.title.trim(),template.body]).map_err(err)?;
                Ok(Reply::Saved { id: template.id })
            }
            Request::DeleteTemplate { id } => {
                changed(
                    self.conn
                        .execute("DELETE FROM templates WHERE id=?1", [&id])
                        .map_err(err)?,
                    "Template",
                )?;
                Ok(Reply::Saved { id })
            }
            Request::SaveSettings { settings } => {
                if !["paper", "carbon"].contains(&settings.theme.as_str()) {
                    return Err("Choose the Paper or Carbon theme.".into());
                }
                if !(1..=MAX_ACTIVE_RUNS).contains(&settings.max_active_runs) {
                    return Err("Choose between one and four active runs.".into());
                }
                bounded(&settings.grok_path, 4096, "Grok executable path")?;
                if !settings.grok_path.is_empty() && !Path::new(&settings.grok_path).is_absolute() {
                    return Err("The Grok executable path must be absolute.".into());
                }
                self.conn
                    .execute(
                        "UPDATE settings SET value=?1 WHERE key='app'",
                        [serde_json::to_string(&settings).map_err(err)?],
                    )
                    .map_err(err)?;
                Ok(Reply::Saved {
                    id: "settings".into(),
                })
            }
            Request::MarkDecision {
                thread_id,
                message_id,
            } => {
                let tx = self
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(err)?;
                let body: Option<String> = tx
                    .query_row(
                        "SELECT body FROM messages WHERE id=?1 AND thread_id=?2",
                        params![message_id, thread_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(err)?;
                let body =
                    body.ok_or("The decision's message was not found in this conversation.")?;
                let summary: String = body.chars().take(200).collect();
                let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM activities WHERE thread_id=?1 AND kind='decision' AND source_id=?2)",params![thread_id,message_id],|r|r.get(0)).map_err(err)?;
                if !exists {
                    record(&tx, &thread_id, "decision", &summary, &message_id)?;
                }
                tx.commit().map_err(err)?;
                Ok(Reply::Saved { id: message_id })
            }
            Request::QueueRun {
                command_id,
                thread_id,
                prompt,
            } => self
                .queue_run(&command_id, &thread_id, &prompt)
                .map(|id| Reply::Saved { id }),
            Request::CancelRun { run_id } => self
                .cancel_run(&run_id)
                .map(|_| Reply::Saved { id: run_id }),
            Request::ResolveRun { .. } => Err(
                "Reconcile the provider through the desktop host before releasing a workspace."
                    .into(),
            ),
            Request::ReadOutput {
                run_id,
                role,
                offset,
            } => {
                let (text, next_offset) = self.read_output(&run_id, &role, offset)?;
                Ok(Reply::Output {
                    run_id,
                    role,
                    offset,
                    text,
                    next_offset,
                })
            }
            _ => Err("This request requires the desktop host.".into()),
        }
    }

    fn queue_run(&mut self, command: &str, thread: &str, prompt: &str) -> Result<String> {
        required(command, 128, "Command identifier")?;
        required(prompt, LOCAL_LIMIT, "Prompt")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let receipt: Option<(String, String, String)> = tx
            .query_row(
                "SELECT run_id,thread_id,prompt FROM receipts WHERE command_id=?1",
                [command],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(err)?;
        if let Some((run, original_thread, original_prompt)) = receipt {
            if original_thread != thread || original_prompt != prompt {
                return Err(
                    "This command identifier was already used for a different request.".into(),
                );
            }
            return Ok(run);
        }
        let path: Option<String> = tx
            .query_row(
                "SELECT p.path FROM projects p JOIN threads t ON t.project_id=p.id WHERE t.id=?1",
                [thread],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        let path = path.ok_or("Conversation was not found.")?;
        let (workspace, lease_key) = canonical_workspace(Path::new(&path))?;
        if workspace != path {
            return Err(
                "The workspace path has changed. Reopen it as a project before running an agent."
                    .into(),
            );
        }
        let run = id();
        let message = id();
        tx.execute("INSERT INTO runs(id,command_id,thread_id,workspace,lease_key,prompt,status,created_at) VALUES(?1,?2,?3,?4,?5,?6,'queued',?7)",params![run,command,thread,workspace,lease_key,prompt,now()]).map_err(err)?;
        tx.execute(
            "INSERT INTO receipts(command_id,run_id,thread_id,prompt) VALUES(?1,?2,?3,?4)",
            params![command, run, thread, prompt],
        )
        .map_err(err)?;
        tx.execute("INSERT INTO messages(id,thread_id,run_id,role,body,created_at) VALUES(?1,?2,?3,'user',?4,?5)",params![message,thread,run,prompt,now()]).map_err(err)?;
        let title: String = prompt
            .lines()
            .next()
            .unwrap_or(prompt)
            .chars()
            .take(72)
            .collect();
        tx.execute("UPDATE threads SET draft='',title=CASE WHEN title='New conversation' THEN ?1 ELSE title END WHERE id=?2",params![title,thread]).map_err(err)?;
        record(&tx, thread, "queued", "Agent task queued.", &run)?;
        tx.commit().map_err(err)?;
        Ok(run)
    }

    pub fn next_runnable(&mut self) -> Result<Option<Run>> {
        let max = self.settings()?.max_active_runs.min(MAX_ACTIVE_RUNS);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let active: i64 = tx
            .query_row(
                &format!("SELECT count(*) FROM runs WHERE status IN ({ACTIVE})"),
                [],
                |r| r.get(0),
            )
            .map_err(err)?;
        if active >= max as i64 {
            return Ok(None);
        }
        let leases: Vec<PathBuf> = {
            let mut stmt = tx.prepare("SELECT workspace FROM leases").map_err(err)?;
            stmt.query_map([], |r| r.get::<_, String>(0).map(PathBuf::from))
                .map_err(err)?
                .collect::<rusqlite::Result<_>>()
                .map_err(err)?
        };
        let candidates: Vec<(String, String, String, String)> = {
            let mut stmt=tx.prepare("SELECT r.id,r.thread_id,r.workspace,r.lease_key FROM runs r WHERE r.status='queued' AND NOT EXISTS(SELECT 1 FROM leases l WHERE l.workspace=r.lease_key) ORDER BY r.created_at,r.rowid").map_err(err)?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(err)?
                .collect::<rusqlite::Result<_>>()
                .map_err(err)?
        };
        for (run, thread, workspace, key) in candidates {
            if leases
                .iter()
                .any(|p| p.starts_with(&key) || Path::new(&key).starts_with(p))
            {
                continue;
            }
            match canonical_workspace(Path::new(&workspace)) {
                Ok((current, current_key)) if current == workspace && current_key == key => {}
                _ => {
                    tx.execute("UPDATE runs SET status='failed',error='Workspace changed or became unavailable before launch.' WHERE id=?1",[&run]).map_err(err)?;
                    record(
                        &tx,
                        &thread,
                        "failed",
                        "Workspace unavailable; agent was not launched.",
                        &run,
                    )?;
                    continue;
                }
            }
            tx.execute(
                "INSERT INTO leases(workspace,run_id) VALUES(?1,?2)",
                params![key, run],
            )
            .map_err(err)?;
            tx.execute("UPDATE runs SET status='starting',session_id=(SELECT session_id FROM threads WHERE id=?2) WHERE id=?1",params![run,thread]).map_err(err)?;
            record(
                &tx,
                &thread,
                "starting",
                "Workspace reserved; agent is starting.",
                &run,
            )?;
            let result = tx
                .query_row(
                    &format!("SELECT {RUN_COLS} FROM runs WHERE id=?1"),
                    [&run],
                    run_row,
                )
                .map_err(err)?;
            tx.commit().map_err(err)?;
            return Ok(Some(result));
        }
        tx.commit().map_err(err)?;
        Ok(None)
    }

    pub fn set_session(&mut self, run_id: &str, session_id: &str) -> Result<()> {
        required(session_id, 512, "Provider session identifier")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let (thread, status): (String, String) = tx
            .query_row(
                "SELECT thread_id,status FROM runs WHERE id=?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(err)?;
        if !["starting", "running", "waiting", "cancelling"].contains(&status.as_str()) {
            return Err("This run is no longer active.".into());
        }
        tx.execute("UPDATE runs SET session_id=?1,status=CASE WHEN status='cancelling' THEN status ELSE 'running' END WHERE id=?2",params![session_id,run_id]).map_err(err)?;
        tx.execute(
            "UPDATE threads SET session_id=?1 WHERE id=?2",
            params![session_id, thread],
        )
        .map_err(err)?;
        record(
            &tx,
            &thread,
            "session",
            "Provider session connected.",
            run_id,
        )?;
        tx.commit().map_err(err)
    }

    pub fn append_output(&mut self, run_id: &str, role: &str, text: &str) -> Result<()> {
        if !["assistant", "tool"].contains(&role) {
            return Err("Unsupported provider message type.".into());
        }
        if text.is_empty() {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let (thread, status): (String, String) = tx
            .query_row(
                "SELECT thread_id,status FROM runs WHERE id=?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(err)?;
        if !["starting", "running", "waiting", "cancelling"].contains(&status.as_str()) {
            return Err("Output arrived for a run that is no longer active.".into());
        }
        save_chunks(&tx, run_id, role, text)?;
        let message = format!("{run_id}:{role}");
        let previous: Option<String> = tx
            .query_row("SELECT body FROM messages WHERE id=?1", [&message], |r| {
                r.get(0)
            })
            .optional()
            .map_err(err)?;
        let mut body = previous.unwrap_or_default();
        if body.ends_with(OUTPUT_NOTICE) {
            return tx.commit().map_err(err);
        }
        let available = BODY_LIMIT.saturating_sub(body.len());
        if text.len() > available {
            let mut end = available.saturating_sub(OUTPUT_NOTICE.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if available < OUTPUT_NOTICE.len() {
                let mut keep = BODY_LIMIT - OUTPUT_NOTICE.len();
                while !body.is_char_boundary(keep) {
                    keep -= 1;
                }
                body.truncate(keep);
            } else {
                body.push_str(&text[..end]);
            }
            body.push_str(OUTPUT_NOTICE);
            record(
                &tx,
                &thread,
                "output_limit",
                "Message preview shortened at 256 KiB; full output remains saved locally.",
                &message,
            )?;
        } else {
            body.push_str(text);
        }
        tx.execute("INSERT INTO messages(id,thread_id,run_id,role,body,created_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET body=excluded.body",params![message,thread,run_id,role,body,now()]).map_err(err)?;
        tx.commit().map_err(err)
    }

    fn output_contains(&self, run_id: &str, role: &str, needle: &str) -> Result<bool> {
        if !["assistant", "tool"].contains(&role) {
            return Ok(false);
        }
        let mut statement = self
            .conn
            .prepare("SELECT body FROM output_chunks WHERE run_id=?1 AND role=?2 ORDER BY offset")
            .map_err(err)?;
        let mut rows = statement.query(params![run_id, role]).map_err(err)?;
        let mut window = String::new();
        while let Some(row) = rows.next().map_err(err)? {
            let mut chunk: String = row.get(0).map_err(err)?;
            chunk.make_ascii_lowercase();
            window.push_str(&chunk);
            if window.contains(needle) {
                return Ok(true);
            }
            let mut keep_from = window.len().saturating_sub(needle.len().saturating_sub(1));
            while !window.is_char_boundary(keep_from) {
                keep_from += 1;
            }
            window.drain(..keep_from);
        }
        Ok(false)
    }

    pub fn read_output(
        &self,
        run_id: &str,
        role: &str,
        offset: u64,
    ) -> Result<(String, Option<u64>)> {
        if !["assistant", "tool"].contains(&role) {
            return Err("Unsupported provider message type.".into());
        }
        self.run(run_id)?;
        let offset =
            i64::try_from(offset).map_err(|_| "Output offset is too large.".to_string())?;
        let total: i64 = self.conn.query_row(
            "SELECT COALESCE((SELECT offset+byte_length FROM output_chunks WHERE run_id=?1 AND role=?2 ORDER BY offset DESC LIMIT 1),0)",
            params![run_id, role], |r| r.get(0),
        ).map_err(err)?;
        if offset > total {
            return Err("Output offset is beyond the saved output.".into());
        }
        if offset == total {
            return Ok((String::new(), None));
        }
        let mut statement = self.conn.prepare("SELECT offset,body FROM output_chunks WHERE run_id=?1 AND role=?2 AND offset>=COALESCE((SELECT offset FROM output_chunks WHERE run_id=?1 AND role=?2 AND offset<=?3 ORDER BY offset DESC LIMIT 1),0) ORDER BY offset LIMIT 256").map_err(err)?;
        let mut rows = statement
            .query(params![run_id, role, offset])
            .map_err(err)?;
        let mut position = offset;
        let mut text = String::with_capacity(OUTPUT_PAGE);
        while let Some(row) = rows.next().map_err(err)? {
            let start: i64 = row.get(0).map_err(err)?;
            let chunk: String = row.get(1).map_err(err)?;
            let relative = usize::try_from(position - start)
                .map_err(|_| "Saved output contains a gap.".to_string())?;
            if relative > chunk.len() || !chunk.is_char_boundary(relative) {
                return Err("Output offset does not identify a UTF-8 character boundary.".into());
            }
            let mut take = (chunk.len() - relative).min(OUTPUT_PAGE - text.len());
            while !chunk.is_char_boundary(relative + take) {
                take -= 1;
            }
            text.push_str(&chunk[relative..relative + take]);
            position += take as i64;
            if text.len() == OUTPUT_PAGE || relative + take < chunk.len() {
                break;
            }
        }
        if text.is_empty() && position < total {
            return Err("Saved output could not be read completely.".into());
        }
        Ok((text, (position < total).then_some(position as u64)))
    }

    pub fn finish_run(
        &mut self,
        run_id: &str,
        status: &str,
        error: Option<&str>,
        usage: Option<Usage>,
    ) -> Result<()> {
        if !["completed", "cancelled", "failed", "interrupted", "unknown"].contains(&status) {
            return Err("Unsupported final run state.".into());
        }
        if let Some(error) = error {
            bounded(error, 4096, "Run error")?;
        }
        if usage
            .as_ref()
            .is_some_and(|u| u.cost_usd.is_some_and(|c| !c.is_finite() || c < 0.))
        {
            return Err("Provider reported an invalid cost.".into());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let (thread, previous): (String, String) = tx
            .query_row(
                "SELECT thread_id,status FROM runs WHERE id=?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(err)?;
        if ["completed", "cancelled", "failed", "interrupted"].contains(&previous.as_str()) {
            if previous == status {
                return Ok(());
            }
            return Err("This run already has a final outcome.".into());
        }
        if previous == "unknown" && status != "unknown" {
            return Err("Reconcile the unknown run before changing its outcome.".into());
        }
        let usage = usage
            .map(|u| serde_json::to_string(&u).map_err(err))
            .transpose()?;
        tx.execute(
            "UPDATE runs SET status=?1,error=?2,usage=COALESCE(?3,usage) WHERE id=?4",
            params![status, error, usage, run_id],
        )
        .map_err(err)?;
        if status != "unknown" {
            tx.execute("DELETE FROM leases WHERE run_id=?1", [run_id])
                .map_err(err)?;
        }
        record(
            &tx,
            &thread,
            status,
            &format!("Agent task {status}."),
            run_id,
        )?;
        tx.commit().map_err(err)
    }

    pub fn cancel_run(&mut self, run_id: &str) -> Result<Run> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let run = tx
            .query_row(
                &format!("SELECT {RUN_COLS} FROM runs WHERE id=?1"),
                [run_id],
                run_row,
            )
            .map_err(err)?;
        match run.status.as_str() {
            "queued" => {
                tx.execute("UPDATE runs SET status='cancelled' WHERE id=?1", [run_id])
                    .map_err(err)?;
                record(
                    &tx,
                    &run.thread_id,
                    "cancelled",
                    "Queued task cancelled before launch.",
                    run_id,
                )?;
            }
            "starting" | "running" | "waiting" => {
                tx.execute("UPDATE runs SET status='cancelling' WHERE id=?1", [run_id])
                    .map_err(err)?;
                record(
                    &tx,
                    &run.thread_id,
                    "cancelling",
                    "Stop requested; awaiting provider outcome.",
                    run_id,
                )?;
            }
            "unknown" => {
                return Err(
                    "The provider outcome is unknown. Reconcile it before releasing the workspace."
                        .into(),
                );
            }
            _ => {}
        }
        tx.commit().map_err(err)?;
        self.run(run_id)
    }

    pub fn resolve_run(&mut self, run_id: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let (thread, status): (String, String) = tx
            .query_row(
                "SELECT thread_id,status FROM runs WHERE id=?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(err)?;
        if status != "unknown" {
            return Err("Only an unknown run requires reconciliation.".into());
        }
        tx.execute("UPDATE runs SET status='interrupted',error='You confirmed the external task has stopped. This interrupted task was not resubmitted.' WHERE id=?1",[run_id]).map_err(err)?;
        tx.execute("DELETE FROM leases WHERE run_id=?1", [run_id])
            .map_err(err)?;
        record(
            &tx,
            &thread,
            "reconciled",
            "You confirmed the external task stopped; workspace released without resubmission.",
            run_id,
        )?;
        tx.commit().map_err(err)
    }
}

#[cfg(test)]
mod tests;
