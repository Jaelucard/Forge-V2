mod panels;
mod state;

use forge_domain::*;
use leptos::prelude::*;
use state::*;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(|| view! { <App/> });
}

#[component]
fn App() -> impl IntoView {
    let ui = Ui::new();
    provide_context(ui);
    ui.refresh(true);
    ui.check_provider();
    Effect::new(move |_| {
        let active = ui
            .snapshot
            .with(|s| s.runs.iter().any(|r| is_active(&r.status)));
        if active && !ui.polling.get_untracked() {
            ui.polling.set(true);
            leptos::task::spawn_local(async move {
                loop {
                    gloo_timers::future::TimeoutFuture::new(700).await;
                    if !ui
                        .snapshot
                        .with_untracked(|s| s.runs.iter().any(|r| is_active(&r.status)))
                    {
                        break;
                    }
                    ui.load(false).await;
                }
                ui.polling.set(false);
            });
        }
    });
    view! {
        <div class="app" data-theme=move || ui.snapshot.with(|s| s.settings.theme.clone())>
            <a class="skip-link" href="#composer">"Skip to composer"</a>
            <Sidebar/>
            <main class="main-shell">
                <header class="topbar">
                    <div class="breadcrumb"><span class="eyebrow">"PERSONAL WORKSPACE"</span><span class="slash">"/"</span><span>{move || ui.project().map(|p| p.name).unwrap_or_else(|| "Start here".into())}</span></div>
                    <div class="top-actions">
                        <button class="quiet small" on:click=move |_| ui.toggle_theme() aria-label="Switch Paper and Carbon theme">{move || if ui.snapshot.with(|s| s.settings.theme == "carbon") { "◐ Carbon" } else { "◑ Paper" }}</button>
                        <button class="quiet small" on:click=move |_| ui.open_panel("settings")>"Settings"</button>
                    </div>
                </header>
                <Show when=move || !ui.error.get().is_empty()>
                    <div class="banner error" role="alert"><span>{move || ui.error.get()}</span><button class="quiet small" aria-label="Dismiss error" on:click=move |_| ui.error.set(String::new())>"Dismiss"</button></div>
                </Show>
                <Show when=move || !ui.notice.get().is_empty()>
                    <div class="banner notice" role="status"><span>{move || ui.notice.get()}</span><button class="quiet small" aria-label="Dismiss notification" on:click=move |_| ui.notice.set(String::new())>"×"</button></div>
                </Show>
                <Show when=move || ui.loading.get() fallback=move || view! {
                    <Show when=move || ui.thread().is_some() fallback=move || view! { <Welcome/> }>
                        <Session/>
                    </Show>
                }>
                    <div class="loading-state"><span class="eyebrow">"FORGE V2"</span><h1>"Opening your workspace."</h1><p>"Loading locally saved sessions and notes."</p></div>
                </Show>
                <ResourceStrip/>
            </main>
            <panels::Drawer/>
        </div>
    }
}

#[component]
fn Sidebar() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let search = RwSignal::new(String::new());
    view! {
        <aside class="sidebar" aria-label="Workspace navigation">
            <div class="brand"><div class="brand-word">"FORGE"<span class="brand-version">"V2"</span></div><div class="brand-rule"></div><p>"A place to make things."</p></div>
            <button class="primary add-project" on:click=move |_| ui.open_panel("project")>"＋ Add workspace"</button>
            <div class="sidebar-heading"><span class="eyebrow">"YOUR WORKSPACES"</span><span class="count">{move || ui.snapshot.with(|s| s.projects.len())}</span></div>
            <label class="sr-only" for="navigation-search">"Filter sessions"</label>
            <input id="navigation-search" class="search-input" placeholder="Find a session…" prop:value=move || search.get() on:input=move |e| search.set(event_target_value(&e))/>
            <nav class="project-list">
                {move || ui.snapshot.with(|s| s.projects.clone()).into_iter().filter(|p| {
                    let q = search.get().to_lowercase();
                    q.is_empty() || p.name.to_lowercase().contains(&q) || p.brief.to_lowercase().contains(&q) || ui.snapshot.with(|s| s.threads.iter().any(|t| t.project_id == p.id && t.title.to_lowercase().contains(&q)))
                }).map(move |project| {
                    let project_id = project.id.clone();
                    let project_matches = project.name.to_lowercase().contains(&search.get().to_lowercase()) || project.brief.to_lowercase().contains(&search.get().to_lowercase());
                    let project_for_new = project.id.clone();
                    let project_path = project.path.clone();
                    view! {
                        <section class="project-group">
                            <div class="project-heading"><span title=project_path>{project.name}</span><button class="icon-button" title="New session" aria-label="New session in this workspace" on:click=move |_| ui.new_thread(project_for_new.clone())>"＋"</button></div>
                            <div class="thread-list">
                                {move || ui.snapshot.with(|s| s.threads.iter().filter(|t| t.project_id == project_id && (project_matches || search.get().is_empty() || t.title.to_lowercase().contains(&search.get().to_lowercase()))).cloned().collect::<Vec<_>>()).into_iter().map(move |thread| {
                                    let id = thread.id.clone();
                                    let active_id = thread.id.clone();
                                    view! { <button class="thread-row" class:selected=move || ui.selected.get().as_ref() == Some(&active_id) on:click=move |_| ui.select_thread(id.clone())><span class="thread-dot">"·"</span><span>{thread.title}</span></button> }
                                }).collect_view()}
                            </div>
                        </section>
                    }
                }).collect_view()}
                <Show when=move || ui.snapshot.with(|s| s.projects.is_empty())><p class="sidebar-empty">"Your projects and conversations will live here."</p></Show>
            </nav>
            <div class="sidebar-bottom">
                <button class="sidebar-tool" on:click=move |_| ui.open_panel("prompts")><span>"↗"</span>"Prompt library"</button>
                <button class="sidebar-tool" on:click=move |_| ui.open_panel("settings")><span class="provider-dot" class:connected=move || ui.provider.with(|p| p.as_ref().is_some_and(|p| p.available))></span><span>"Grok Build"</span><span class="muted">{move || ui.provider.with(|p| if p.as_ref().is_some_and(|p| p.available) { "CLI found" } else { "Set up" })}</span></button>
                <p class="local-label">"LOCAL HISTORY · YOUR WORKSPACE"</p>
            </div>
        </aside>
    }
}

#[component]
fn Welcome() -> impl IntoView {
    let ui = expect_context::<Ui>();
    view! {
        <section class="welcome">
            <div class="edition-line"><span>"THE PERSONAL EDITION"</span><span>"01 / WORKSPACE"</span></div>
            <h1>"Make room"<br/>"for your next idea"<span class="red-period">"."</span></h1>
            <p class="welcome-intro">"A focused place to code, write, research, and keep the thinking around your work."</p>
            <button class="primary large" on:click=move |_| ui.open_panel("project")>"Choose a workspace"<span>"↗"</span></button>
            <div class="welcome-bottom">
                <div><span class="eyebrow">"START WITH A FOLDER"</span><p>"Bring an existing project, or choose an empty folder for something new."</p></div>
                <div><span class="eyebrow">"KEEP THE CONTEXT"</span><p>"Briefs, margin notes, and reusable prompts stay with your work."</p></div>
                <div><span class="eyebrow">"WORK AT YOUR PACE"</span><p>"Focus on a conversation. Open Review or Explore when you need more."</p></div>
            </div>
        </section>
    }
}

#[component]
fn Session() -> impl IntoView {
    let ui = expect_context::<Ui>();
    view! {
        <div class="session-heading">
            <div><div class="eyebrow">"WORKING SESSION"</div><h1>{move || ui.thread().map(|t| t.title).unwrap_or_default()}</h1></div>
            <div class="mode-switch" aria-label="Workspace layout">
                {["focus", "review", "explore"].into_iter().map(move |mode| view! { <button aria-pressed=move || (ui.layout() == mode).to_string() class:active=move || ui.layout() == mode on:click=move |_| ui.set_layout(mode)>{title_case(mode)}</button> }).collect_view()}
            </div>
        </div>
        <div class="session-tools"><button on:click=move |_| ui.open_panel("brief")>"Project brief"</button><button on:click=move |_| { ui.note_anchor.set(None); ui.open_panel("notes"); }>"Margin notes "<span class="count">{move || ui.snapshot.with(|s| s.notes.len())}</span></button><button on:click=move |_| ui.open_panel("recap")>"Session recap"</button><div class="tool-spacer"></div><button on:click=move |_| ui.open_panel("prompts")>"Prompts ↗"</button></div>
        <div class="session-body" class:split=move || ui.layout() != "focus">
            <section class="conversation" aria-label="Conversation">
                <div class="messages" id="conversation">
                    <HistoryControls/>
                    <Show when=move || ui.snapshot.with(|s| s.messages.is_empty())>
                        <div class="conversation-empty"><span class="eyebrow">"A NEW PAGE"</span><h2>"What are we working on?"</h2><p>"Set an intention, ask a question, or start with a prompt from your library."</p><button class="text-button" on:click=move |_| ui.open_panel("brief")>"Write a project brief ↗"</button></div>
                    </Show>
                    <For each=move || ui.snapshot.with(|s| s.messages.clone()) key=|m| (m.id.clone(), m.body.len()) children=move |message| view! { <MessageView message/> }/>
                </div>
                <RunList/>
                <Composer/>
            </section>
            <Show when=move || ui.layout() != "focus"><panels::WorkspacePanel/></Show>
        </div>
    }
}

#[component]
fn HistoryControls() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let query = RwSignal::new(String::new());
    view! {
        <div class="history-controls">
            <form class="history-search" on:submit=move |e| { e.prevent_default(); ui.search.set(query.get_untracked()); ui.before.set(None); ui.refresh(false); }>
                <label class="sr-only" for="history-search">"Search this session"</label><input id="history-search" placeholder="Search this session…" prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e))/><button class="quiet small" type="submit">"Find"</button>
            </form>
            <Show when=move || ui.snapshot.with(|s| s.has_older_messages)><button class="quiet small" on:click=move |_| { ui.before.set(ui.snapshot.with_untracked(|s| s.messages.first().map(|m| m.sequence))); ui.refresh(false); }>"Earlier messages ↑"</button></Show>
            <Show when=move || ui.before.get().is_some() || !ui.search.get().is_empty()><button class="quiet small" on:click=move |_| { ui.before.set(None); ui.search.set(String::new()); query.set(String::new()); ui.refresh(false); }>"Back to latest ↓"</button></Show>
        </div>
    }
}

#[component]
fn MessageView(message: Message) -> impl IntoView {
    let ui = expect_context::<Ui>();
    let note_id = message.id.clone();
    let decision_id = message.id.clone();
    let thread_id = message.thread_id.clone();
    let is_user = message.role == "user";
    let output_run = message.run_id.clone();
    let output_role = message.role.clone();
    let role = if is_user {
        "YOU"
    } else if message.role == "assistant" {
        "GROK"
    } else {
        "ACTIVITY"
    };
    view! {
        <article id=format!("message-{}", message.id) class="message" class:user-message=is_user>
            <header><span class="message-role">{role}</span><span class="message-sequence">{format!("{:02}", message.sequence)}</span></header>
            <div class="message-body">{message.body}</div>
            <div class="message-actions"><button on:click=move |_| { ui.note_anchor.set(Some(note_id.clone())); ui.open_panel("notes"); }>"＋ Note"</button><button on:click=move |_| ui.mutate(Request::MarkDecision { thread_id: thread_id.clone(), message_id: decision_id.clone() }, "Marked as a decision.")>"Mark decision"</button>{(!is_user).then(move || output_run.map(|run_id| view! { <button on:click=move |_| { ui.output.set(None); ui.output_history.set(Vec::new()); ui.read_output(run_id.clone(), output_role.clone(), 0); }>"Read full output ↗"</button> }))}</div>
        </article>
    }
}

#[component]
fn RunList() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let runs = Memo::new(move |_| {
        let selected = ui.selected.get();
        ui.snapshot.with(|snapshot| {
            let latest_failure = snapshot
                .runs
                .iter()
                .filter(|run| {
                    Some(&run.thread_id) == selected.as_ref()
                        && matches!(run.status.as_str(), "failed" | "interrupted")
                })
                .max_by_key(|run| (run.created_at, run.id.as_str()))
                .map(|run| run.id.as_str());
            snapshot
                .runs
                .iter()
                .filter(|run| {
                    Some(&run.thread_id) == selected.as_ref()
                        && (is_active(&run.status)
                            || run.status == "unknown"
                            || Some(run.id.as_str()) == latest_failure)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    let questions = Memo::new(move |_| {
        ui.snapshot.with(|s| {
            s.questions
                .iter()
                .filter(|q| {
                    s.runs.iter().any(|r| {
                        r.id == q.run_id && Some(&r.thread_id) == ui.selected.get().as_ref()
                    })
                })
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    view! {
        <div class="run-list" class:has-content=move || !runs.get().is_empty() || !questions.get().is_empty() aria-label="Current run controls" aria-live="polite">
            <For each=move || questions.get() key=|q| (q.run_id.clone(), q.request_id.clone()) children=move |question| view! { <ProviderQuestion question/> }/>
            {move || runs.get().into_iter().map(move |run| {
                let cancel = run.id.clone();
                let resolve = run.id.clone();
                let active = is_active(&run.status);
                let unknown = run.status == "unknown";
                let cancelling = run.status == "cancelling";
                view! {
                    <div class="run-status"><div><span class="eyebrow">{run_label(&run.status)}</span>{run.error.map(|e| view! { <p>{e}</p> })}</div>
                        {active.then(move || view! { <button class="quiet small" disabled=cancelling on:click=move |_| ui.mutate(Request::CancelRun { run_id: cancel.clone() }, "Stop requested.")>{if cancelling { "Stopping…" } else { "Stop" }}</button> })}
                        {unknown.then(move || view! { <div class="recovery"><p>"Check Grok separately before releasing this workspace. Forge cannot confirm what happened after it lost contact."</p><button class="quiet small" on:click=move |_| ui.mutate(Request::ResolveRun { run_id: resolve.clone() }, "Run resolved. Its workspace is available again.")>"Confirm external run has stopped"</button></div> })}
                    </div>
                }
            }).collect_view()}
        </div>
    }
}

#[component]
fn ProviderQuestion(question: PermissionQuestion) -> impl IntoView {
    use std::collections::HashMap;
    let ui = expect_context::<Ui>();
    let answers = RwSignal::new(HashMap::<String, Vec<String>>::new());
    let freeform = RwSignal::new(HashMap::<String, String>::new());
    let is_input = question.kind == "user_input";
    let options = question.options;
    let questions = question.questions;
    let question_ids = questions.iter().map(|q| q.id.clone()).collect::<Vec<_>>();
    let submit_ids = question_ids.clone();
    let run_id = question.run_id.clone();
    let request_id = question.request_id.clone();
    let submit_run = question.run_id;
    let submit_request = question.request_id;
    view! {
        <div class="permission-question">
            <span class="eyebrow">"GROK NEEDS YOUR INPUT"</span><p>{question.title}</p>
            {(!is_input).then(move || view! { <div class="button-row">{options.into_iter().map(move |option| { let run_id = run_id.clone(); let request_id = request_id.clone(); view! { <button class="quiet" on:click=move |_| ui.mutate(Request::AnswerPermission { run_id: run_id.clone(), request_id: request_id.clone(), option_id: option.id.clone() }, "Response sent.")>{option.label}</button> } }).collect_view()}</div> })}
            {is_input.then(move || view! {
                <form on:submit=move |event| {
                    event.prevent_default();
                    let mut result = answers.get_untracked();
                    for (id, value) in freeform.get_untracked() { if !value.trim().is_empty() { result.entry(id).or_default().push(value.trim().into()); } }
                    if submit_ids.iter().any(|id| result.get(id).is_none_or(Vec::is_empty)) { return; }
                    ui.mutate(Request::AnswerUserInput { run_id: submit_run.clone(), request_id: submit_request.clone(), answers: result }, "Response sent.");
                }>
                    {questions.into_iter().map(move |q| {
                        let free_id = q.id.clone();
                        let read_free_id = q.id.clone();
                        let question_id = q.id.clone();
                        let multi = q.multi_select;
                        view! { <fieldset><legend>{q.question}</legend>{q.options.into_iter().map(move |option| {
                            let group = question_id.clone(); let write = question_id.clone(); let read = question_id.clone(); let read_option = option.clone(); let write_option = option.clone();
                            view! { <label class="question-option"><input type=if multi { "checkbox" } else { "radio" } name=group prop:checked=move || answers.with(|a| a.get(&read).is_some_and(|v| v.contains(&read_option))) on:change=move |e| {
                                let checked = event_target_checked(&e);
                                answers.update(|a| { let list = a.entry(write.clone()).or_default(); if !multi { list.clear(); } list.retain(|v| v != &write_option); if checked { list.push(write_option.clone()); } });
                                if !multi { freeform.update(|f| { f.remove(&write); }); }
                            }/><span>{option}</span></label> }
                        }).collect_view()}<label class="freeform-answer">"Write a response"<input prop:value=move || freeform.with(|f| f.get(&read_free_id).cloned().unwrap_or_default()) on:input=move |e| { let value = event_target_value(&e); freeform.update(|f| { f.insert(free_id.clone(), value); }); if !multi { answers.update(|a| { a.remove(&free_id); }); } }/></label></fieldset> }
                    }).collect_view()}
                    <button class="primary" type="submit" disabled=move || question_ids.iter().any(|id| !answers.with(|a| a.get(id).is_some_and(|v| !v.is_empty())) && !freeform.with(|f| f.get(id).is_some_and(|v| !v.trim().is_empty())))>"Send response"</button>
                </form>
            })}
        </div>
    }
}

#[component]
fn Composer() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let running = Memo::new(move |_| {
        ui.snapshot.with(|s| {
            s.runs
                .iter()
                .find(|r| {
                    Some(&r.thread_id) == ui.selected.get().as_ref()
                        && matches!(
                            r.status.as_str(),
                            "starting" | "running" | "waiting" | "cancelling"
                        )
                })
                .map(|r| (r.id.clone(), r.status.clone()))
        })
    });
    view! {
        <div class="composer-wrap">
            <Show when=move || ui.provider.with(|p| p.as_ref().is_some_and(|p| !p.available))><div class="setup-nudge"><span>"Connect Grok Build to send your first message."</span><button class="text-button" on:click=move |_| ui.open_panel("settings")>"Set up ↗"</button></div></Show>
            <div class="composer-box">
                <label class="sr-only" for="composer">"Message Grok"</label>
                <textarea id="composer" placeholder="Describe what you want to make…" rows="4" prop:value=move || ui.draft.get() on:input=move |e| ui.edit_draft(event_target_value(&e)) on:keydown=move |e| { if (e.meta_key() || e.ctrl_key()) && e.key() == "Enter" && !e.is_composing() { e.prevent_default(); ui.send(); } }></textarea>
                <div class="composer-footer"><div class="composer-meta"><span class="provider-dot"></span><strong>"Grok Build"</strong><span class="muted">{move || if ui.dirty.get() { "Saving draft…" } else { "Draft saved locally" }}</span></div><div class="composer-actions"><Show when=move || running.get().is_some()><button class="quiet stop" disabled=move || running.get().is_some_and(|(_, status)| status == "cancelling") on:click=move |_| if let Some((run_id, _)) = running.get_untracked() { ui.mutate(Request::CancelRun { run_id }, "Stop requested."); }>{move || if running.get().is_some_and(|(_, status)| status == "cancelling") { "Stopping…" } else { "Stop" }}</button></Show><button class="primary send" disabled=move || ui.busy.get() || ui.draft.get().trim().is_empty() on:click=move |_| ui.send()>{move || if ui.busy.get() { "Sending…" } else { "Send ↑" }}</button></div></div>
            </div>
            <div class="composer-caption"><span>"Only Send starts agent work."</span><span>"⌘ / Ctrl + Enter"</span></div>
        </div>
    }
}

#[component]
fn ResourceStrip() -> impl IntoView {
    let ui = expect_context::<Ui>();
    view! {
        <footer class="resource-strip"><div><span class="status-square"></span><span>"Saved on this device"</span><span class="resource-separator">"/"</span><span>{move || format!("{} active · {} queued", ui.snapshot.with(|s| s.runs.iter().filter(|r| matches!(r.status.as_str(), "running" | "starting" | "cancelling" | "waiting")).count()), ui.snapshot.with(|s| s.runs.iter().filter(|r| r.status == "queued").count()))}</span></div><button on:click=move |_| { ui.open_panel("resources"); ui.measure_resources(); }>"Usage & resources ↗"</button></footer>
    }
}
