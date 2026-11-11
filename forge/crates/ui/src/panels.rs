use crate::state::*;
use forge_domain::*;
use leptos::prelude::*;
use std::collections::BTreeMap;
use wasm_bindgen::JsCast;

#[component]
pub fn Drawer() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let dialog = NodeRef::<leptos::html::Aside>::new();
    let previous = std::rc::Rc::new(std::cell::RefCell::new(None::<web_sys::Element>));
    let was_open = std::cell::Cell::new(false);
    Effect::new(move |_| {
        let open = !ui.panel.get().is_empty();
        if open {
            if let Some(element) = dialog.get()
                && !was_open.replace(true)
            {
                *previous.borrow_mut() = web_sys::window()
                    .and_then(|w| w.document())
                    .and_then(|d| d.active_element());
                let _ = element.focus();
            }
        } else if was_open.replace(false)
            && let Some(element) = previous
                .borrow_mut()
                .take()
                .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = element.focus();
        }
    });
    view! {
        <Show when=move || !ui.panel.get().is_empty()>
            <div class="drawer-scrim" on:click=move |_| ui.panel.set(String::new())></div>
            <aside class="drawer" role="dialog" aria-modal="true" aria-label=move || title_case(&ui.panel.get()) tabindex="-1" node_ref=dialog on:keydown=move |e| {
                if e.key() == "Escape" { ui.panel.set(String::new()); return; }
                if e.key() == "Tab"
                    && let Some(element) = dialog.get_untracked()
                    && let Ok(nodes) = element.query_selector_all("button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), summary, a[href]") {
                            let controls = (0..nodes.length()).filter_map(|index| nodes.item(index).and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok())).collect::<Vec<_>>();
                            let active = web_sys::window().and_then(|w| w.document()).and_then(|d| d.active_element());
                            if let (Some(first), Some(last)) = (controls.first(), controls.last()) {
                                let at_first = active.as_ref().is_some_and(|a| a.is_same_node(Some(first.as_ref())));
                                let at_last = active.as_ref().is_some_and(|a| a.is_same_node(Some(last.as_ref())));
                                let at_dialog = active.as_ref().is_some_and(|a| a.is_same_node(Some(element.as_ref())));
                                if e.shift_key() && (at_first || at_dialog) { e.prevent_default(); let _ = last.focus(); }
                                else if !e.shift_key() && (at_last || at_dialog) { e.prevent_default(); let _ = first.focus(); }
                            }
                }
            }>
                <div class="drawer-header"><span class="eyebrow">"YOUR WORKSPACE / "{move || ui.panel.get().to_uppercase()}</span><button class="icon-button" aria-label="Close panel" on:click=move |_| ui.panel.set(String::new())>"×"</button></div>
                <div class="drawer-content">
                    <Show when=move || !ui.error.get().is_empty()><p class="panel-error" role="alert">{move || ui.error.get()}</p></Show>
                    {move || match ui.panel.get().as_str() {
                        "project" => view! { <ProjectForm/> }.into_any(),
                        "brief" => view! { <BriefPanel/> }.into_any(),
                        "notes" => view! { <NotesPanel/> }.into_any(),
                        "recap" => view! { <RecapPanel/> }.into_any(),
                        "prompts" => view! { <PromptsPanel/> }.into_any(),
                        "settings" => view! { <SettingsPanel/> }.into_any(),
                        "resources" => view! { <ResourcesPanel/> }.into_any(),
                        "output" => view! { <OutputPanel/> }.into_any(),
                        _ => ().into_any(),
                    }}
                </div>
            </aside>
        </Show>
    }
}

#[component]
fn OutputPanel() -> impl IntoView {
    let ui = expect_context::<Ui>();
    view! {
        <h2>"The complete record."</h2><p class="panel-intro">"Saved provider output, loaded one page at a time."</p>
        {move || ui.output.get().map(|OutputPage { run_id, role, offset, text, next_offset: next }| {
            let back_run = run_id.clone(); let back_role = role.clone();
            view! {
                <div class="output-pagination"><button class="quiet small" disabled=move || ui.output_history.get().is_empty() on:click=move |_| { let previous = ui.output_history.try_update(|history| history.pop()).flatten(); if let Some(previous) = previous { ui.read_output(back_run.clone(), back_role.clone(), previous); } }>"← Previous"</button><span class="eyebrow">{format!("OFFSET {offset}")}</span><button class="quiet small" disabled=next.is_none() on:click=move |_| if let Some(next) = next { ui.output_history.update(|history| history.push(offset)); ui.read_output(run_id.clone(), role.clone(), next); }>"Next →"</button></div>
                <pre class="saved-output">{if text.is_empty() { "No output was recorded for this page.".into() } else { text }}</pre>
            }
        })}
    }
}

#[component]
fn ProjectForm() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let name = RwSignal::new(String::new());
    let path = RwSignal::new(String::new());
    let saving = RwSignal::new(false);
    let pick = move |_| {
        leptos::task::spawn_local(async move {
            match dispatch(Request::PickFolder).await {
                Ok(Reply::Folder(Some(folder))) => {
                    if name.get_untracked().is_empty() {
                        name.set(
                            folder
                                .rsplit(['/', '\\'])
                                .next()
                                .unwrap_or("Workspace")
                                .to_string(),
                        );
                    }
                    path.set(folder);
                }
                Ok(_) => {}
                Err(error) => ui.error.set(error),
            }
        })
    };
    view! {
        <h2>"A home for your work."</h2><p class="panel-intro">"Choose a folder. Forge keeps its conversations and notes locally, alongside a clear connection to your project."</p>
        <form on:submit=move |event| {
            event.prevent_default();
            if saving.get_untracked() { return; }
            saving.set(true);
            leptos::task::spawn_local(async move {
                match dispatch(Request::AddProject { name: name.get_untracked(), path: path.get_untracked() }).await {
                    Ok(Reply::Saved { id }) => { ui.panel.set(String::new()); ui.new_thread(id); },
                    Ok(_) => ui.refresh(true),
                    Err(error) => ui.error.set(error),
                }
                saving.set(false);
            });
        }>
            <label for="workspace-name">"Workspace name"</label><input id="workspace-name" required placeholder="My next project" prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e))/>
            <label for="workspace-path">"Project folder"</label><div class="field-with-button"><input id="workspace-path" required placeholder="/path/to/your/project" prop:value=move || path.get() on:input=move |e| path.set(event_target_value(&e))/><button type="button" class="quiet" on:click=pick>"Choose…"</button></div>
            <p class="field-help">"An existing folder on this computer. Your files are kept in place."</p>
            <button class="primary full-width" disabled=move || saving.get() || name.get().trim().is_empty() || path.get().trim().is_empty() type="submit">{move || if saving.get() { "Opening…" } else { "Open workspace ↗" }}</button>
        </form>
    }
}

#[component]
fn BriefPanel() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let brief = RwSignal::new(ui.project().map(|p| p.brief).unwrap_or_default());
    view! {
        <h2>"Keep the intention clear."</h2><p class="panel-intro">"An objective, a few constraints, and what done looks like. Your brief is saved for this project."</p>
        <Show when=move || ui.project().is_some() fallback=|| view! { <p>"Open a session to write its project brief."</p> }>
            <label for="project-brief">"Project brief"</label><textarea id="project-brief" rows="15" placeholder="What are you making?\n\nWhat matters most?\n\nWhat will a good result look like?" prop:value=move || brief.get() on:input=move |e| brief.set(event_target_value(&e))></textarea>
            <button class="primary" on:click=move |_| if let Some(project) = ui.project() { ui.mutate(Request::SaveBrief { project_id: project.id, brief: brief.get_untracked() }, "Project brief saved."); }>"Save brief"</button>
            <div class="panel-divider"></div><span class="eyebrow">"PINNED NOTES"</span>
            {move || ui.snapshot.with(|s| s.notes.iter().filter(|n| n.pinned).cloned().collect::<Vec<_>>()).into_iter().map(|note| view! { <p class="pinned-note">{note.body}</p> }).collect_view()}
            <p class="field-help">"Pin tasks or decisions from Margin notes to keep them close. Saving a brief makes no provider request."</p>
        </Show>
    }
}

#[component]
fn NotesPanel() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let body = RwSignal::new(String::new());
    let editing = RwSignal::new(None::<String>);
    let pinned = RwSignal::new(false);
    let filter = RwSignal::new(String::new());
    view! {
        <h2>"Think in the margins."</h2><p class="panel-intro">"Capture your own observations, follow-ups, and decisions as the work develops."</p>
        <Show when=move || ui.thread().is_some() fallback=|| view! { <p>"Open a session to add a note."</p> }>
            <Show when=move || ui.note_anchor.get().is_some()><div class="anchor-label">"Attached to a message"<button class="text-button" on:click=move |_| ui.note_anchor.set(None)>"Remove link"</button></div></Show>
            <label for="note-body">{move || if editing.get().is_some() { "Edit note" } else { "New note" }}</label><textarea id="note-body" rows="5" placeholder="Something worth remembering…" prop:value=move || body.get() on:input=move |e| body.set(event_target_value(&e))></textarea>
            <label class="checkbox-label"><input type="checkbox" prop:checked=move || pinned.get() on:change=move |e| pinned.set(event_target_checked(&e))/>"Pin to project brief"</label>
            <div class="button-row"><button class="primary" disabled=move || body.get().trim().is_empty() on:click=move |_| {
                if let Some(thread_id) = ui.selected.get_untracked() {
                    let note = Note { id: editing.get_untracked().unwrap_or_default(), thread_id, message_id: ui.note_anchor.get_untracked(), body: body.get_untracked(), pinned: pinned.get_untracked(), created_at: 0 };
                    leptos::task::spawn_local(async move { match dispatch(Request::SaveNote { note }).await { Ok(_) => { body.set(String::new()); editing.set(None); ui.note_anchor.set(None); pinned.set(false); ui.load(false).await; }, Err(error) => ui.error.set(error) } });
                }
            }>"Save note"</button><Show when=move || editing.get().is_some()><button class="quiet" on:click=move |_| { editing.set(None); body.set(String::new()); pinned.set(false); ui.note_anchor.set(None); }>"Cancel edit"</button></Show></div>
            <div class="panel-divider"></div><label class="sr-only" for="notes-filter">"Find a note"</label><input id="notes-filter" placeholder="Find a note…" prop:value=move || filter.get() on:input=move |e| filter.set(event_target_value(&e))/>
            {move || ui.snapshot.with(|s| s.notes.iter().filter(|n| n.body.to_lowercase().contains(&filter.get().to_lowercase())).cloned().collect::<Vec<_>>()).into_iter().map(move |note| {
                let edit = note.clone(); let delete = note.id.clone(); let pin = note.clone();
                view! { <article class="note-card"><div class="eyebrow">{if note.pinned { "PINNED NOTE" } else { "MARGIN NOTE" }}</div><p>{note.body}</p><div class="button-row"><button class="text-button" on:click=move |_| { body.set(edit.body.clone()); editing.set(Some(edit.id.clone())); pinned.set(edit.pinned); ui.note_anchor.set(edit.message_id.clone()); }>"Edit"</button><button class="text-button" on:click=move |_| { let mut note = pin.clone(); note.pinned = !note.pinned; ui.mutate(Request::SaveNote { note }, ""); }>{if note.pinned { "Unpin" } else { "Pin" }}</button><button class="text-button" on:click=move |_| ui.mutate(Request::DeleteNote { id: delete.clone() }, "Note deleted.")>"Delete"</button></div></article> }
            }).collect_view()}
        </Show>
    }
}

#[component]
fn RecapPanel() -> impl IntoView {
    let ui = expect_context::<Ui>();
    view! {
        <h2>"The work, on record."</h2><p class="panel-intro">"A record of saved activity and the decisions you marked. Each item keeps its original reference."</p>
        <div class="recap-key"><span class="status-square"></span>"Built from recorded activity. No AI summary."</div>
        <Show when=move || ui.snapshot.with(|s| s.activities.is_empty())><p class="empty-copy">"Activity will appear here as you work. Mark a message as a decision to give it a place in this record."</p></Show>
        <ol class="recap-list">{move || ui.snapshot.with(|s| s.activities.clone()).into_iter().map(move |activity| {
            let source_id = activity.source_id.clone();
            let run = ui.snapshot.with_untracked(|s| s.runs.iter().find(|r| r.id == activity.source_id).cloned());
            let message = ui.snapshot.with_untracked(|s| s.messages.iter().find(|m| m.id == activity.source_id).cloned());
            view! {
                <li><span class="eyebrow">{activity.kind.replace('_', " ")}</span><p>{activity.summary}</p><details><summary>"Recorded evidence"</summary><div class="evidence"><span>"Event "{activity.id}</span><code>{source_id}</code>{run.map(|r| view! { <p>"Run state: "{r.status}</p> })}{message.map(|m| view! { <blockquote>{m.body}</blockquote> })}</div></details></li>
            }
        }).collect_view()}</ol>
        <p class="field-help">"A recorded completion is a provider report. Tests and command outcomes are unknown unless present in the saved result."</p>
    }
}

fn variables(body: &str) -> Vec<String> {
    let mut variables = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("}}") else {
            break;
        };
        let name = rest[..end].trim().to_string();
        if !name.is_empty() && name.len() <= 64 && !variables.contains(&name) {
            variables.push(name);
        }
        rest = &rest[end + 2..];
    }
    variables
}

fn prepare(body: &str, values: &BTreeMap<String, String>) -> String {
    let mut output = String::new();
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            output.push_str(&rest[start..]);
            return output;
        };
        match values.get(after[..end].trim()).filter(|v| !v.is_empty()) {
            Some(value) => output.push_str(value),
            None => output.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    output.push_str(rest);
    output
}

#[component]
fn PromptsPanel() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let query = RwSignal::new(String::new());
    let selected = RwSignal::new(String::new());
    let title = RwSignal::new(String::new());
    let body = RwSignal::new(String::new());
    let values = RwSignal::new(BTreeMap::<String, String>::new());
    view! {
        <h2>"A useful starting point."</h2><p class="panel-intro">"Keep prompts you return to. Fill in the details, then move a draft into your conversation."</p>
        <label class="sr-only" for="prompt-search">"Search templates"</label><input id="prompt-search" placeholder="Find a prompt…" prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e))/>
        <div class="template-list">{move || ui.snapshot.with(|s| s.templates.iter().filter(|t| format!("{} {}", t.title, t.body).to_lowercase().contains(&query.get().to_lowercase())).cloned().collect::<Vec<_>>()).into_iter().map(move |template| {
            let id = template.id.clone();
            view! { <button class="template-row" class:selected=move || selected.get() == id on:click=move |_| { selected.set(template.id.clone()); title.set(template.title.clone()); body.set(template.body.clone()); values.set(BTreeMap::new()); }><span>{template.title.clone()}</span><span>"↗"</span></button> }
        }).collect_view()}</div>
        <button class="text-button" on:click=move |_| { selected.set(String::new()); title.set(String::new()); body.set(String::new()); values.set(BTreeMap::new()); }>"＋ New template"</button>
        <div class="panel-divider"></div><label for="template-title">"Template title"</label><input id="template-title" placeholder="Review a change" prop:value=move || title.get() on:input=move |e| title.set(event_target_value(&e))/>
        <label for="template-body">"Prompt"</label><textarea id="template-body" rows="7" placeholder="Review {{change}} for correctness. Pay attention to {{focus}}." prop:value=move || body.get() on:input=move |e| body.set(event_target_value(&e))></textarea><p class="field-help">"Use {{name}} to add a detail you can fill in each time."</p>
        <div class="button-row"><button class="quiet" disabled=move || title.get().trim().is_empty() || body.get().trim().is_empty() on:click=move |_| { let template = PromptTemplate { id: selected.get_untracked(), title: title.get_untracked(), body: body.get_untracked() }; leptos::task::spawn_local(async move { match dispatch(Request::SaveTemplate { template }).await { Ok(Reply::Saved { id }) => { selected.set(id); ui.notice.set("Template saved.".into()); ui.load(false).await; }, Ok(_) => ui.load(false).await, Err(error) => ui.error.set(error) } }); }>"Save template"</button><Show when=move || !selected.get().is_empty()><button class="text-button" on:click=move |_| { ui.mutate(Request::DeleteTemplate { id: selected.get_untracked() }, "Template deleted."); selected.set(String::new()); title.set(String::new()); body.set(String::new()); }>"Delete"</button></Show></div>
        <For each=move || variables(&body.get()) key=|name| name.clone() children=move |name| { let read = name.clone(); let write = name.clone(); let id = format!("variable-{name}"); view! { <label for=id.clone()>{name}</label><input id=id prop:value=move || values.with(|v| v.get(&read).cloned().unwrap_or_default()) on:input=move |e| { let value = event_target_value(&e); values.update(|v| { v.insert(write.clone(), value); }); }/> } }/>
        <Show when=move || !body.get().is_empty()><div class="panel-divider"></div><span class="eyebrow">"DRAFT PREVIEW"</span><pre class="prompt-preview">{move || prepare(&body.get(), &values.get())}</pre><button class="primary full-width" disabled=move || ui.thread().is_none() on:click=move |_| { ui.edit_draft(prepare(&body.get_untracked(), &values.get_untracked())); ui.panel.set(String::new()); ui.notice.set("Prompt placed in the composer. Review it, then choose Send.".into()); }>"Use in composer ↗"</button><p class="field-help">"This prepares a draft. It does not send a message or run an agent."</p></Show>
    }
}

#[component]
fn SettingsPanel() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let path = RwSignal::new(ui.snapshot.with_untracked(|s| s.settings.grok_path.clone()));
    let slots = RwSignal::new(ui.snapshot.with_untracked(|s| s.settings.max_active_runs));
    view! {
        <h2>"Make it yours."</h2><p class="panel-intro">"Appearance, your Grok connection, and how much work Forge starts at once."</p>
        <label>"Appearance"</label><div class="theme-options">{["paper", "carbon"].into_iter().map(move |theme| view! { <button class="theme-choice" class:chosen=move || ui.snapshot.with(|s| s.settings.theme == theme) on:click=move |_| { let mut settings = ui.snapshot.with_untracked(|s| s.settings.clone()); settings.theme = theme.into(); ui.mutate(Request::SaveSettings { settings }, ""); }><span class=format!("theme-swatch {theme}")></span><span>{title_case(theme)}</span></button> }).collect_view()}</div>
        <div class="panel-divider"></div><div class="panel-section-heading"><h3>"Grok Build"</h3><button class="text-button" on:click=move |_| ui.check_provider()>"Check connection"</button></div>
        {move || ui.provider.get().map(|status| view! { <div class="provider-status"><strong>{if status.available { "CLI found" } else { "Setup needed" }}</strong><p>{status.message}</p><p class="field-help">{match status.auth_status.as_str() { "not_checked" => "Sign-in is checked when you send a message.", "authenticated" => "Signed in with Grok Build.", "unauthenticated" | "login_required" => "Sign in with Grok Build before sending a message.", _ => "Sign-in status is unavailable. Grok will check when you send a message." }}</p>{status.version.map(|v| view! { <code>{v}</code> })}</div> })}
        <label for="grok-path">"Grok Build executable"</label><input id="grok-path" placeholder="Automatic discovery" prop:value=move || path.get() on:input=move |e| path.set(event_target_value(&e))/><p class="field-help">"Use your installed Grok Build CLI and its existing sign-in. The Grok Bot desktop app is a separate product."</p>
        <details class="connection-help"><summary>"Connection and workspace permissions"</summary>{move || ui.provider.get().map(|p| view! { <p>{p.workspace_authority}</p><p>{p.fan_out}</p> })}<p>"Forge does not import browser cookies or ask for an API key. Your provider's own sign-in manages your account."</p></details>
        <label for="run-slots">"Active agent runs"</label><select id="run-slots" prop:value=move || slots.get().to_string() on:change=move |e| slots.set(event_target_value(&e).parse().unwrap_or(4))>{(1..=4).map(|n| view! { <option value=n.to_string()>{format!("{n} at a time")}</option> }).collect_view()}</select><p class="field-help">"One writer per checkout. Other work waits in the queue. Provider-owned subagents may not be controllable by Forge."</p>
        <button class="primary full-width" on:click=move |_| {
            let mut settings = ui.snapshot.with_untracked(|s| s.settings.clone()); settings.grok_path = path.get_untracked(); settings.max_active_runs = slots.get_untracked();
            leptos::task::spawn_local(async move { match dispatch(Request::SaveSettings { settings }).await { Ok(_) => { ui.load(false).await; ui.check_provider(); ui.notice.set("Settings saved.".into()); }, Err(error) => ui.error.set(error) } });
        }>"Save connection settings"</button>
    }
}

#[component]
fn ResourcesPanel() -> impl IntoView {
    let ui = expect_context::<Ui>();
    view! {
        <h2>"Know what's in use."</h2><p class="panel-intro">"A snapshot taken when you ask. Usage comes from provider reports; unavailable totals stay unknown."</p><button class="quiet" on:click=move |_| ui.measure_resources()>"Refresh measurements"</button>
        {move || ui.resources.get().map(|r| view! { <dl class="resource-grid"><div><dt>"App process memory"</dt><dd>{bytes_label(r.app_rss_bytes)}</dd></div><div><dt>"Provider processes"</dt><dd>{bytes_label(r.provider_rss_bytes)}</dd></div><div><dt>"Active runs"</dt><dd>{r.active_runs}</dd></div><div><dt>"Waiting in queue"</dt><dd>{r.queued_runs}</dd></div></dl><p class="field-help">{r.message}</p> })}
        <div class="panel-divider"></div><h3>"Reported session usage"</h3>
        {move || {
            let runs = ui.snapshot.with(|s| s.runs.iter().filter(|r| Some(&r.thread_id) == ui.selected.get().as_ref()).cloned().collect::<Vec<_>>());
            let input = runs.iter().filter_map(|r| r.usage.input_tokens).sum::<u64>();
            let output = runs.iter().filter_map(|r| r.usage.output_tokens).sum::<u64>();
            let has_input = runs.iter().any(|r| r.usage.input_tokens.is_some());
            let has_output = runs.iter().any(|r| r.usage.output_tokens.is_some());
            let cost = runs.iter().filter_map(|r| r.usage.cost_usd).sum::<f64>();
            let has_cost = runs.iter().any(|r| r.usage.cost_usd.is_some());
            view! { <dl class="resource-grid"><div><dt>"Input tokens"</dt><dd>{if has_input { input.to_string() } else { "Not reported".into() }}</dd></div><div><dt>"Output tokens"</dt><dd>{if has_output { output.to_string() } else { "Not reported".into() }}</dd></div><div><dt>"Reported cost"</dt><dd>{if has_cost { format!("${cost:.4}") } else { "Not reported".into() }}</dd></div></dl><p class="field-help">"Partial reports are not a complete billing total. Subscription allowance is managed by Grok."</p> }
        }}
    }
}

#[component]
pub fn WorkspacePanel() -> impl IntoView {
    let ui = expect_context::<Ui>();
    let filter = RwSignal::new(String::new());
    if ui.workspace.get_untracked().is_none() {
        ui.load_workspace(String::new());
    }
    view! {
        <aside class="workspace-panel" aria-label="Workspace files and changes">
            <div class="workspace-panel-header"><span class="eyebrow">{move || if ui.layout() == "review" { "CHANGE REVIEW" } else { "EXPLORE WORKSPACE" }}</span><button class="text-button" on:click=move |_| ui.load_workspace(ui.directory.get_untracked())>"Refresh"</button></div>
            <div class="workspace-content">
                {move || ui.workspace.get().map(|workspace| view! {
                    <div class="branch-line"><span>"↳"</span><span>{workspace.branch.unwrap_or_else(|| "Local folder".into())}</span></div>
                    <Show when=move || ui.layout() == "review" fallback=move || view! {
                        <div class="file-navigation"><button class="text-button" on:click=move |_| ui.load_workspace(String::new())>"Project root"</button><span>{move || ui.directory.get()}</span><Show when=move || !ui.directory.get().is_empty()><button class="text-button" on:click=move |_| { let directory = ui.directory.get_untracked(); let parent = directory.rsplit_once('/').map(|(p, _)| p).unwrap_or("").to_string(); ui.load_workspace(parent); }>"↑ Up"</button></Show></div>
                        <label class="sr-only" for="file-filter">"Filter files"</label><input id="file-filter" placeholder="Find a file in this folder…" prop:value=move || filter.get() on:input=move |e| filter.set(event_target_value(&e))/>
                        <div class="file-list">{move || ui.workspace.with(|w| w.as_ref().map(|w| w.files.iter().filter(|f| f.name.to_lowercase().contains(&filter.get().to_lowercase())).cloned().collect::<Vec<_>>()).unwrap_or_default()).into_iter().map(move |file| view! { <button class="file-row" on:click=move |_| if file.is_dir { ui.load_workspace(file.path.clone()); } else { ui.read_file(file.path.clone()); }><span>{if file.is_dir { "▸" } else { "·" }}</span><span>{file.name}</span></button> }).collect_view()}</div>
                        {move || ui.file.get().map(|(path, text, truncated)| view! { <div class="file-preview"><header><span class="eyebrow">{path}</span><button class="text-button" on:click=move |_| ui.file.set(None)>"Close"</button></header><pre>{text}</pre>{truncated.then(|| view! { <p class="field-help">"This preview is truncated to keep the workspace responsive."</p> })}</div> })}
                    }>
                        <h3 class="small-heading">"Working tree"</h3><pre class="git-status">{if workspace.status.trim().is_empty() { "No recorded changes.".into() } else { workspace.status.clone() }}</pre>
                        <div class="diff-view">{workspace.diff.lines().map(|line| { let class = if line.starts_with('+') && !line.starts_with("+++") { "addition" } else if line.starts_with('-') && !line.starts_with("---") { "deletion" } else if line.starts_with("@@") { "hunk" } else { "context" }; view! { <div class=class>{if line.is_empty() { " ".to_string() } else { line.to_string() }}</div> } }).collect_view()}</div>
                    </Show>
                    {workspace.truncated.then(|| view! { <p class="field-help">"Some content is omitted from this bounded preview."</p> })}
                })}
            </div>
        </aside>
    }
}

#[cfg(test)]
mod tests {
    use super::{prepare, variables};
    use std::collections::BTreeMap;

    #[test]
    fn templates_keep_unknown_or_unclosed_placeholders_and_do_not_reexpand_insertions() {
        let values = BTreeMap::from([
            ("topic".into(), "{{private}}".into()),
            ("empty".into(), String::new()),
        ]);
        assert_eq!(
            prepare(
                "Review {{ topic }} with {{unknown}} and {{empty}}; {{unfinished",
                &values
            ),
            "Review {{private}} with {{unknown}} and {{empty}}; {{unfinished"
        );
        assert_eq!(
            variables("{{topic}} {{ topic }} {{}} {{context}} {{unfinished"),
            vec!["topic", "context"]
        );
    }
}
