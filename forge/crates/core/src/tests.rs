use super::*;
use tempfile::TempDir;

fn setup() -> (TempDir, Store) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("history.sqlite")).unwrap();
    (temp, store)
}
fn saved(reply: Reply) -> String {
    match reply {
        Reply::Saved { id } => id,
        _ => panic!("Expected saved ID"),
    }
}
fn project(store: &mut Store, base: &Path, name: &str) -> String {
    let path = base.join(name);
    std::fs::create_dir_all(&path).unwrap();
    saved(
        store
            .execute(Request::AddProject {
                name: name.into(),
                path: path.to_str().unwrap().into(),
            })
            .unwrap(),
    )
}
fn thread(store: &mut Store, project: &str) -> String {
    saved(
        store
            .execute(Request::CreateThread {
                project_id: project.into(),
                title: String::new(),
            })
            .unwrap(),
    )
}
fn queue(store: &mut Store, thread: &str, command: &str) -> String {
    saved(
        store
            .execute(Request::QueueRun {
                command_id: command.into(),
                thread_id: thread.into(),
                prompt: "Inspect the README without changes.".into(),
            })
            .unwrap(),
    )
}

#[test]
fn local_features_and_drafts_survive_restart_without_any_provider_dependency() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "example");
    let t = thread(&mut store, &p);
    store
        .execute(Request::SaveBrief {
            project_id: p.clone(),
            brief: "Build a quiet personal workspace.\n- [ ] Test on Mac".into(),
        })
        .unwrap();
    store
        .execute(Request::SaveDraft {
            thread_id: t.clone(),
            draft: "Draft is not submitted.".into(),
        })
        .unwrap();
    store
        .execute(Request::SetLayout {
            thread_id: t.clone(),
            layout: "explore".into(),
        })
        .unwrap();
    store
        .execute(Request::SaveSettings {
            settings: Settings {
                theme: "carbon".into(),
                max_active_runs: 2,
                ..Default::default()
            },
        })
        .unwrap();
    store
        .execute(Request::SaveNote {
            note: Note {
                thread_id: t.clone(),
                body: "A local margin note".into(),
                pinned: true,
                ..Default::default()
            },
        })
        .unwrap();
    store
        .execute(Request::SaveTemplate {
            template: PromptTemplate {
                id: String::new(),
                title: "Review".into(),
                body: "Review {{file}} for {{goal}}.".into(),
            },
        })
        .unwrap();
    drop(store);
    let store = Store::open(&temp.path().join("history.sqlite")).unwrap();
    let snap = store.snapshot(Some(&t), None, None).unwrap();
    assert_eq!(snap.settings.theme, "carbon");
    assert_eq!(snap.settings.max_active_runs, 2);
    assert_eq!(snap.threads[0].draft, "Draft is not submitted.");
    assert_eq!(snap.threads[0].layout, "explore");
    assert_eq!(snap.notes[0].body, "A local margin note");
    assert!(snap.notes[0].pinned);
    assert_eq!(snap.templates[0].body, "Review {{file}} for {{goal}}.");
    assert!(snap.projects[0].brief.contains("quiet"));
    assert!(snap.runs.is_empty());
    assert!(snap.messages.is_empty());
    assert_eq!(store.run_counts().unwrap(), (0, 0));
    assert!(!include_str!("../Cargo.toml").contains("forge-grok"));
}

#[test]
fn receipts_deduplicate_exact_requests_and_reject_command_reuse() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let first = queue(&mut store, &t, "stable-id");
    assert_eq!(queue(&mut store, &t, "stable-id"), first);
    let altered = store.execute(Request::QueueRun {
        command_id: "stable-id".into(),
        thread_id: t.clone(),
        prompt: "A different operation".into(),
    });
    assert!(altered.unwrap_err().contains("different request"));
    let snapshot = store.snapshot(Some(&t), None, None).unwrap();
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(
        snapshot
            .activities
            .iter()
            .filter(|e| e.kind == "queued")
            .count(),
        1
    );
    drop(store);
    let mut store = Store::open(&temp.path().join("history.sqlite")).unwrap();
    assert_eq!(queue(&mut store, &t, "stable-id"), first);
    assert_eq!(store.run_counts().unwrap(), (0, 1));
}

#[test]
fn failed_queue_transaction_keeps_the_draft_and_has_no_partial_receipt() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    store
        .execute(Request::SaveDraft {
            thread_id: t.clone(),
            draft: "Keep this draft".into(),
        })
        .unwrap();
    store.conn.execute_batch("CREATE TRIGGER fail_activity BEFORE INSERT ON activities BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;").unwrap();
    assert!(
        store
            .execute(Request::QueueRun {
                command_id: "command".into(),
                thread_id: t.clone(),
                prompt: "Do not partially queue this".into()
            })
            .is_err()
    );
    let snapshot = store.snapshot(Some(&t), None, None).unwrap();
    assert!(snapshot.runs.is_empty());
    assert!(snapshot.messages.is_empty());
    assert!(snapshot.activities.is_empty());
    assert_eq!(snapshot.threads[0].draft, "Keep this draft");
    let receipts: i64 = store
        .conn
        .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(receipts, 0);
    store
        .conn
        .execute_batch("DROP TRIGGER fail_activity;")
        .unwrap();
    queue(&mut store, &t, "command");
    assert_eq!(store.run_counts().unwrap(), (0, 1));
}

#[test]
fn concurrent_limit_and_writer_leases_queue_without_starting_extra_runs() {
    let (temp, mut store) = setup();
    let mut expected = Vec::new();
    for i in 0..5 {
        let p = project(&mut store, temp.path(), &format!("project-{i}"));
        let t = thread(&mut store, &p);
        expected.push(queue(&mut store, &t, &format!("command-{i}")));
    }
    for expected_id in expected.iter().take(4) {
        assert_eq!(store.next_runnable().unwrap().unwrap().id, *expected_id);
    }
    assert!(store.next_runnable().unwrap().is_none());
    assert_eq!(store.run_counts().unwrap(), (4, 1));
    store
        .finish_run(&expected[0], "completed", None, None)
        .unwrap();
    assert_eq!(store.next_runnable().unwrap().unwrap().id, expected[4]);
    assert_eq!(store.run_counts().unwrap(), (4, 0));
}

#[test]
fn writers_to_one_checkout_share_a_lease_including_nested_folders() {
    let (temp, mut store) = setup();
    let root = temp.path().join("checkout");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("subdir")).unwrap();
    let p1 = project(&mut store, temp.path(), "checkout");
    let p2 = project(&mut store, &root, "subdir");
    let t1 = thread(&mut store, &p1);
    let t2 = thread(&mut store, &p2);
    let a = queue(&mut store, &t1, "a");
    let b = queue(&mut store, &t2, "b");
    assert_eq!(store.next_runnable().unwrap().unwrap().id, a);
    assert!(store.next_runnable().unwrap().is_none());
    assert_eq!(store.run(&b).unwrap().status, "queued");
    store.finish_run(&a, "completed", None, None).unwrap();
    assert_eq!(store.next_runnable().unwrap().unwrap().id, b);
}

#[test]
fn overlapping_non_git_folders_are_serialized_in_both_admission_orders() {
    for child_first in [false, true] {
        let (temp, mut store) = setup();
        let root = temp.path().join("workspace");
        let parent = project(&mut store, temp.path(), "workspace");
        let child = project(&mut store, &root, "nested");
        let parent_thread = thread(&mut store, &parent);
        let child_thread = thread(&mut store, &child);
        let order = if child_first {
            [&child_thread, &parent_thread]
        } else {
            [&parent_thread, &child_thread]
        };
        let first = queue(&mut store, order[0], "first");
        let second = queue(&mut store, order[1], "second");
        assert_eq!(store.next_runnable().unwrap().unwrap().id, first);
        assert!(store.next_runnable().unwrap().is_none());
        assert_eq!(store.run(&second).unwrap().status, "queued");
        store.finish_run(&first, "completed", None, None).unwrap();
        assert_eq!(store.next_runnable().unwrap().unwrap().id, second);
    }
}

#[test]
fn distinct_linked_worktrees_can_run_concurrently_with_one_shared_git_directory() {
    let (temp, mut store) = setup();
    let common = temp.path().join("main/.git/worktrees");
    std::fs::create_dir_all(&common).unwrap();
    let mut expected = Vec::new();
    for name in ["one", "two"] {
        let root = temp.path().join(name);
        let p = project(&mut store, temp.path(), name);
        std::fs::write(
            root.join(".git"),
            format!("gitdir: {}\n", common.join(name).display()),
        )
        .unwrap();
        let t = thread(&mut store, &p);
        expected.push(queue(&mut store, &t, name));
    }
    assert_eq!(store.next_runnable().unwrap().unwrap().id, expected[0]);
    assert_eq!(store.next_runnable().unwrap().unwrap().id, expected[1]);
    assert_eq!(store.run_counts().unwrap(), (2, 0));
}

#[cfg(unix)]
#[test]
fn symlink_aliases_do_not_create_separate_workspace_leases() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "real");
    std::os::unix::fs::symlink(temp.path().join("real"), temp.path().join("alias")).unwrap();
    let alias = saved(
        store
            .execute(Request::AddProject {
                name: "alias".into(),
                path: temp.path().join("alias").to_str().unwrap().into(),
            })
            .unwrap(),
    );
    assert_eq!(alias, p);
    let one = thread(&mut store, &p);
    let two = thread(&mut store, &alias);
    queue(&mut store, &one, "a");
    queue(&mut store, &two, "b");
    store.next_runnable().unwrap().unwrap();
    assert!(store.next_runnable().unwrap().is_none());
}

#[test]
fn restart_retains_unknown_writer_lease_until_explicit_reconciliation() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let a = queue(&mut store, &t, "a");
    let b = queue(&mut store, &t, "b");
    store.next_runnable().unwrap().unwrap();
    store.set_session(&a, "provider-session-1").unwrap();
    store
        .append_output(&a, "assistant", "Partial result")
        .unwrap();
    drop(store);
    let mut store = Store::open(&temp.path().join("history.sqlite")).unwrap();
    assert_eq!(store.run(&a).unwrap().status, "unknown");
    assert_eq!(
        store.run(&a).unwrap().session_id.as_deref(),
        Some("provider-session-1")
    );
    assert_eq!(store.run_counts().unwrap(), (1, 1));
    assert!(store.next_runnable().unwrap().is_none());
    assert!(store.finish_run(&a, "completed", None, None).is_err());
    assert!(
        store
            .execute(Request::ResolveRun { run_id: a.clone() })
            .is_err()
    );
    assert!(store.cancel_run(&a).is_err());
    store.resolve_run(&a).unwrap();
    let next = store.next_runnable().unwrap().unwrap();
    assert_eq!(next.id, b);
    assert_eq!(next.session_id.as_deref(), Some("provider-session-1"));
    let snapshot = store.snapshot(Some(&t), None, None).unwrap();
    assert!(snapshot.messages.iter().any(|m| m.body == "Partial result"));
    assert_eq!(
        snapshot
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .count(),
        2
    );
}

#[test]
fn cancellation_only_releases_lease_after_confirmed_provider_outcome() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let a = queue(&mut store, &t, "a");
    let b = queue(&mut store, &t, "b");
    store.next_runnable().unwrap().unwrap();
    assert_eq!(store.cancel_run(&a).unwrap().status, "cancelling");
    assert!(store.next_runnable().unwrap().is_none());
    assert_eq!(store.cancel_run(&b).unwrap().status, "cancelled");
    assert_eq!(store.run_counts().unwrap(), (1, 0));
    store
        .append_output(&a, "assistant", "Final streamed bytes")
        .unwrap();
    store.finish_run(&a, "cancelled", None, None).unwrap();
    assert_eq!(store.run_counts().unwrap(), (0, 0));
    assert!(store.append_output(&a, "assistant", "late").is_err());
}

#[test]
fn history_queries_return_only_one_page_out_of_ten_thousand_messages() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let tx = store.conn.transaction().unwrap();
    for index in 0..10_000 {
        tx.execute(
            "INSERT INTO messages(id,thread_id,role,body,created_at) VALUES(?1,?2,'user',?3,0)",
            params![format!("message-{index}"), t, format!("Message {index}")],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let latest = store.snapshot(Some(&t), None, None).unwrap();
    assert_eq!(latest.messages.len(), HISTORY_PAGE);
    assert!(latest.has_older_messages);
    assert_eq!(latest.messages.last().unwrap().body, "Message 9999");
    let previous = store
        .snapshot(Some(&t), Some(latest.messages[0].sequence), None)
        .unwrap();
    assert_eq!(previous.messages.len(), HISTORY_PAGE);
    assert_eq!(previous.messages.last().unwrap().body, "Message 9919");
    assert!(previous.messages.last().unwrap().sequence < latest.messages[0].sequence);
    let first = store.snapshot(Some(&t), Some(40), None).unwrap();
    assert_eq!(first.messages.len(), 39);
    assert!(!first.has_older_messages);
}

#[test]
fn streamed_output_limit_is_utf8_safe_persisted_and_explicit() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let run = queue(&mut store, &t, "command");
    store.next_runnable().unwrap().unwrap();
    store
        .append_output(&run, "assistant", &"编".repeat(BODY_LIMIT))
        .unwrap();
    store
        .append_output(&run, "assistant", "more bytes must not erase the notice")
        .unwrap();
    let snapshot = store.snapshot(Some(&t), None, None).unwrap();
    let saved = snapshot
        .messages
        .iter()
        .find(|m| m.role == "assistant")
        .unwrap();
    assert!(saved.body.len() <= BODY_LIMIT);
    assert!(saved.body.ends_with(OUTPUT_NOTICE));
    assert_eq!(
        snapshot
            .activities
            .iter()
            .filter(|a| a.kind == "output_limit")
            .count(),
        1
    );
    assert!(store.append_output(&run, "user", "wrong role").is_err());
    let mut complete = String::new();
    let mut offset = 0;
    loop {
        let (page, next) = store.read_output(&run, "assistant", offset).unwrap();
        assert!(page.len() <= OUTPUT_PAGE);
        complete.push_str(&page);
        if let Some(next) = next {
            assert!(next > offset);
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(
        complete,
        format!(
            "{}more bytes must not erase the notice",
            "编".repeat(BODY_LIMIT)
        )
    );
}

#[test]
fn session_search_is_literal_pages_history_and_preserves_navigation_on_no_results() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let other = thread(&mut store, &p);
    let tx = store.conn.transaction().unwrap();
    for i in 0..200 {
        let body = if i == 0 {
            "needle%_\\literal".into()
        } else {
            format!("ordinary {i}")
        };
        tx.execute(
            "INSERT INTO messages(id,thread_id,role,body,created_at) VALUES(?1,?2,'user',?3,0)",
            params![format!("search-{i}"), t, body],
        )
        .unwrap();
    }
    tx.execute(r"INSERT INTO messages(id,thread_id,role,body,created_at) VALUES('other-match',?1,'user','needle%_\literal',0)", [&other]).unwrap();
    tx.execute("INSERT INTO messages(id,thread_id,role,body,created_at) VALUES('wildcard-decoy',?1,'user','needleXXliteral',0)", [&t]).unwrap();
    tx.commit().unwrap();
    let found = store
        .snapshot(Some(&t), None, Some("needle%_\\literal"))
        .unwrap();
    assert_eq!(found.messages.len(), 1);
    assert_eq!(found.messages[0].id, "search-0");
    assert_eq!(found.threads.len(), 2);
    assert_eq!(found.selected_thread.as_deref(), Some(t.as_str()));
    let page = store.snapshot(Some(&t), None, Some("ordinary")).unwrap();
    assert_eq!(page.messages.len(), HISTORY_PAGE);
    assert!(page.has_older_messages);
    let older = store
        .snapshot(Some(&t), Some(page.messages[0].sequence), Some("ordinary"))
        .unwrap();
    assert_eq!(older.messages.len(), HISTORY_PAGE);
    assert!(older.messages.last().unwrap().sequence < page.messages[0].sequence);
    let empty = store
        .snapshot(Some(&t), None, Some("no match here"))
        .unwrap();
    assert!(empty.messages.is_empty());
    assert!(!empty.has_older_messages);
    assert_eq!(empty.selected_thread.as_deref(), Some(t.as_str()));
    assert_eq!(empty.threads.len(), 2);
    assert!(empty.threads.iter().any(|thread| thread.id == t));
    assert_eq!(empty.projects[0].id, p);
}

#[test]
fn session_search_reads_full_output_across_large_and_tiny_chunk_boundaries() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let run = queue(&mut store, &t, "run");
    store.next_runnable().unwrap().unwrap();
    let phrase = r"BOUNDARY%_\文MARK";
    let output = format!("{}{phrase} trailing bytes", "x".repeat(OUTPUT_PAGE * 5 - 5));
    store.append_output(&run, "assistant", &output).unwrap();
    let tiny_phrase = r"tiny%_\文needle";
    for ch in tiny_phrase.chars() {
        store
            .append_output(&run, "assistant", &ch.to_string())
            .unwrap();
    }
    for needle in [r"boundary%_\文mark", tiny_phrase] {
        let snapshot = store.snapshot(Some(&t), None, Some(needle)).unwrap();
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(snapshot.messages[0].role, "assistant");
        assert!(!snapshot.messages[0].body.contains(needle));
        assert!(snapshot.messages[0].body.ends_with(OUTPUT_NOTICE));
        assert!(!snapshot.has_older_messages);
    }
    let no_wildcards = store
        .snapshot(Some(&t), None, Some("boundaryXX文mark"))
        .unwrap();
    assert!(no_wildcards.messages.is_empty());
    let standalone_chunk_matches: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM output_chunks WHERE instr(body,?1)>0 OR instr(body,?2)>0",
            params![phrase, tiny_phrase],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(standalone_chunk_matches, 0);
}

#[test]
fn active_and_unknown_runs_remain_visible_behind_hundreds_of_queued_tasks() {
    let (temp, mut store) = setup();
    let mut active = Vec::new();
    let mut first_thread = String::new();
    for i in 0..4 {
        let p = project(&mut store, temp.path(), &format!("project-{i}"));
        let t = thread(&mut store, &p);
        if i == 0 {
            first_thread = t.clone();
        }
        active.push(queue(&mut store, &t, &format!("active-{i}")));
        store.next_runnable().unwrap().unwrap();
    }
    store
        .finish_run(
            &active[0],
            "unknown",
            Some("Unconfirmed provider outcome"),
            None,
        )
        .unwrap();
    for i in 0..125 {
        queue(&mut store, &first_thread, &format!("queued-{i}"));
    }
    for i in 0..125 {
        let id = queue(&mut store, &first_thread, &format!("cancelled-{i}"));
        store.cancel_run(&id).unwrap();
    }
    let snapshot = store.snapshot(Some(&first_thread), None, None).unwrap();
    assert_eq!(
        snapshot
            .runs
            .iter()
            .filter(|r| active.contains(&r.id))
            .count(),
        4
    );
    assert_eq!(
        snapshot
            .runs
            .iter()
            .filter(|r| r.status == "queued")
            .count(),
        100
    );
    assert_eq!(
        snapshot
            .runs
            .iter()
            .filter(|r| r.status == "cancelled")
            .count(),
        100
    );
    assert_eq!(snapshot.runs.len(), 204);
}

#[test]
fn full_output_pages_survive_restart_and_reject_mid_character_offsets() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let run = queue(&mut store, &t, "run");
    store.next_runnable().unwrap().unwrap();
    let expected = format!("{}FINISHED", "🦀文\n".repeat(140_000));
    store.append_output(&run, "tool", &expected).unwrap();
    store.finish_run(&run, "completed", None, None).unwrap();
    drop(store);
    let store = Store::open(&temp.path().join("history.sqlite")).unwrap();
    let mut result = String::new();
    let mut offset = 0;
    loop {
        let (page, next) = store.read_output(&run, "tool", offset).unwrap();
        assert!(!page.is_empty());
        assert!(page.len() <= OUTPUT_PAGE);
        result.push_str(&page);
        if let Some(next) = next {
            assert!(next > offset);
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(result, expected);
    assert!(store.read_output(&run, "tool", 1).is_err());
    assert!(
        store
            .read_output(&run, "tool", expected.len() as u64 + 1)
            .is_err()
    );
    assert!(store.read_output(&run, "tool", u64::MAX).is_err());
    assert_eq!(
        store
            .read_output(&run, "tool", expected.len() as u64)
            .unwrap(),
        (String::new(), None)
    );
    let oversized: i64 = store.conn.query_row("SELECT count(*) FROM output_chunks WHERE byte_length>65536 OR byte_length!=length(CAST(body AS BLOB))", [], |r|r.get(0)).unwrap();
    assert_eq!(oversized, 0);
}

#[test]
fn full_output_and_preview_roll_back_together_if_preview_update_fails() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let run = queue(&mut store, &t, "run");
    store.next_runnable().unwrap().unwrap();
    store
        .append_output(&run, "assistant", "First saved bytes")
        .unwrap();
    store.conn.execute_batch("CREATE TRIGGER fail_preview BEFORE UPDATE ON messages BEGIN SELECT RAISE(ABORT,'injected output failure'); END;").unwrap();
    assert!(
        store
            .append_output(&run, "assistant", &"next".repeat(BODY_LIMIT))
            .is_err()
    );
    assert_eq!(
        store.read_output(&run, "assistant", 0).unwrap(),
        ("First saved bytes".into(), None)
    );
    let snapshot = store.snapshot(Some(&t), None, None).unwrap();
    assert_eq!(
        snapshot
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap()
            .body,
        "First saved bytes"
    );
    assert!(!snapshot.activities.iter().any(|a| a.kind == "output_limit"));
    store
        .conn
        .execute_batch("DROP TRIGGER fail_preview;")
        .unwrap();
    store
        .append_output(&run, "assistant", " then continued")
        .unwrap();
    assert_eq!(
        store.read_output(&run, "assistant", 0).unwrap().0,
        "First saved bytes then continued"
    );
}

#[test]
fn version_one_history_is_backed_up_and_existing_output_is_preserved() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let run = queue(&mut store, &t, "run");
    store.next_runnable().unwrap().unwrap();
    store
        .append_output(&run, "assistant", "Existing trial output 文")
        .unwrap();
    store.finish_run(&run, "completed", None, None).unwrap();
    store
        .conn
        .execute_batch("DROP TABLE output_chunks; PRAGMA user_version=1;")
        .unwrap();
    drop(store);
    let store = Store::open(&temp.path().join("history.sqlite")).unwrap();
    assert_eq!(
        store.read_output(&run, "assistant", 0).unwrap(),
        ("Existing trial output 文".into(), None)
    );
    let backup = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .find(|entry| entry.file_name().to_string_lossy().contains("pre-v2-"))
        .unwrap();
    let conn = Connection::open(backup.path()).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let body: String = conn
        .query_row(
            "SELECT body FROM messages WHERE role='assistant'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(body, "Existing trial output 文");
}

#[test]
fn note_anchors_cannot_cross_threads_and_decisions_link_to_actual_messages() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t1 = thread(&mut store, &p);
    let t2 = thread(&mut store, &p);
    queue(&mut store, &t1, "command");
    let message = store.snapshot(Some(&t1), None, None).unwrap().messages[0]
        .id
        .clone();
    let note = Note {
        thread_id: t2.clone(),
        message_id: Some(message.clone()),
        body: "Wrong anchor".into(),
        ..Default::default()
    };
    assert!(store.execute(Request::SaveNote { note }).is_err());
    assert!(
        store
            .execute(Request::MarkDecision {
                thread_id: t2,
                message_id: message.clone()
            })
            .is_err()
    );
    for _ in 0..2 {
        store
            .execute(Request::MarkDecision {
                thread_id: t1.clone(),
                message_id: message.clone(),
            })
            .unwrap();
    }
    let snapshot = store.snapshot(Some(&t1), None, None).unwrap();
    let decisions: Vec<_> = snapshot
        .activities
        .iter()
        .filter(|a| a.kind == "decision")
        .collect();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].source_id, message);
    assert_eq!(decisions[0].summary, snapshot.messages[0].body);
}

#[test]
fn invalid_user_input_does_not_change_local_state() {
    let (temp, mut store) = setup();
    assert!(
        store
            .execute(Request::AddProject {
                name: "project".into(),
                path: "relative".into()
            })
            .is_err()
    );
    assert!(
        store
            .execute(Request::AddProject {
                name: " ".into(),
                path: temp.path().to_str().unwrap().into()
            })
            .is_err()
    );
    assert!(
        store
            .execute(Request::SaveSettings {
                settings: Settings {
                    max_active_runs: 5,
                    ..Default::default()
                }
            })
            .is_err()
    );
    assert!(
        store
            .execute(Request::SaveSettings {
                settings: Settings {
                    theme: "unrecognised".into(),
                    ..Default::default()
                }
            })
            .is_err()
    );
    assert!(
        store
            .execute(Request::SaveSettings {
                settings: Settings {
                    grok_path: "grok; danger".into(),
                    ..Default::default()
                }
            })
            .is_err()
    );
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    assert!(
        store
            .execute(Request::SetLayout {
                thread_id: t.clone(),
                layout: "hidden".into()
            })
            .is_err()
    );
    assert!(
        store
            .execute(Request::SaveDraft {
                thread_id: t.clone(),
                draft: "a".repeat(LOCAL_LIMIT + 1)
            })
            .is_err()
    );
    assert!(
        store
            .execute(Request::QueueRun {
                command_id: "a".into(),
                thread_id: t.clone(),
                prompt: " ".into()
            })
            .is_err()
    );
    assert!(
        store
            .execute(Request::QueueRun {
                command_id: "a".into(),
                thread_id: "missing".into(),
                prompt: "test".into()
            })
            .is_err()
    );
    let snap = store.snapshot(Some(&t), None, None).unwrap();
    assert_eq!(snap.settings, Settings::default());
    assert!(snap.runs.is_empty());
    assert_eq!(snap.threads[0].draft, "");
    assert_eq!(snap.threads[0].layout, "focus");
}

#[test]
fn unavailable_workspace_fails_before_provider_admission() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let a = queue(&mut store, &t, "a");
    std::fs::rename(temp.path().join("project"), temp.path().join("moved")).unwrap();
    assert!(store.next_runnable().unwrap().is_none());
    assert_eq!(store.run(&a).unwrap().status, "failed");
    assert_eq!(store.run_counts().unwrap(), (0, 0));
}

#[test]
fn schema_guard_rejects_future_and_unrecognised_databases() {
    let temp = tempfile::tempdir().unwrap();
    let future = temp.path().join("future.sqlite");
    Connection::open(&future)
        .unwrap()
        .execute_batch("PRAGMA user_version=9;")
        .unwrap();
    assert!(Store::open(&future).err().unwrap().contains("newer Forge"));
    let foreign = temp.path().join("foreign.sqlite");
    Connection::open(&foreign)
        .unwrap()
        .execute_batch("CREATE TABLE private(value TEXT); INSERT INTO private VALUES('preserved');")
        .unwrap();
    assert!(
        Store::open(&foreign)
            .err()
            .unwrap()
            .contains("not an empty Forge")
    );
    let conn = Connection::open(foreign).unwrap();
    let value: String = conn
        .query_row("SELECT value FROM private", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "preserved");
}

#[test]
fn final_outcomes_are_stable_and_usage_only_comes_from_explicit_reports() {
    let (temp, mut store) = setup();
    let p = project(&mut store, temp.path(), "project");
    let t = thread(&mut store, &p);
    let a = queue(&mut store, &t, "a");
    assert_eq!(store.run(&a).unwrap().usage, Usage::default());
    store.next_runnable().unwrap().unwrap();
    let usage = Usage {
        input_tokens: Some(100),
        output_tokens: Some(23),
        cost_usd: None,
    };
    store
        .finish_run(&a, "completed", None, Some(usage.clone()))
        .unwrap();
    store.finish_run(&a, "completed", None, None).unwrap();
    assert!(
        store
            .finish_run(&a, "failed", Some("late failure"), None)
            .is_err()
    );
    assert_eq!(store.run(&a).unwrap().usage, usage);
    let snap = store.snapshot(Some(&t), None, None).unwrap();
    assert_eq!(
        snap.activities
            .iter()
            .filter(|a| a.kind == "completed")
            .count(),
        1
    );
}
