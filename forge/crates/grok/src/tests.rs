use super::*;
use tempfile::TempDir;

#[test]
fn authentication_respects_the_announced_default_and_never_chooses_api_billing() {
    let mut init = json!({"authMethods":[{"id":"xai.api_key"},{"id":"cached_token"}],"_meta":{"defaultAuthMethodId":"cached_token"}});
    assert!(require_cached_auth(&init).is_ok());
    init["_meta"]["defaultAuthMethodId"] = json!("xai.api_key");
    assert!(require_cached_auth(&init).is_err());
    assert!(require_cached_auth(&json!({"authMethods":[{"id":"browser_login"}]})).is_err());
}

#[test]
fn absent_usage_is_not_zero_and_cost_ticks_are_converted_only_when_reported() {
    assert!(reported_usage(&json!({"sessionUpdate":"turn_completed"})).is_none());
    assert_eq!(
        reported_usage(&json!({"usage":{"inputTokens":5,"costUsdTicks":1250000000u64}})),
        Some(Usage {
            input_tokens: Some(5),
            output_tokens: None,
            cost_usd: Some(0.125)
        })
    );
    assert_eq!(
        reported_usage(&json!({"usage":{"outputTokens":0,"costUsdTicks":-1}})),
        Some(Usage {
            input_tokens: None,
            output_tokens: Some(0),
            cost_usd: None
        })
    );
}

#[tokio::test]
async fn frame_reader_rejects_oversized_invalid_and_incomplete_records() {
    for (input, expected) in [
        (format!("{}\n", "x".repeat(MAX_FRAME)), "1 MiB"),
        ("not JSON\n".into(), "invalid"),
        ("{\"jsonrpc\":\"2.0\"}".into(), "incomplete"),
    ] {
        let (tx, mut rx) = mpsc::channel(2);
        read_frames(BufReader::new(input.as_bytes()), tx).await;
        let error = rx.recv().await.unwrap().unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
}

#[cfg(unix)]
fn fixture(mode: &str, resume: bool) -> (TempDir, GrokRunConfig) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("grok-fixture");
    let source = format!("#!/usr/bin/env python3\nMODE = {mode:?}\n")
        + include_str!("../tests/fixtures/acp.py");
    std::fs::write(&script, source).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = GrokRunConfig {
        executable: script,
        workspace: dir.path().into(),
        prompt: "Protocol fixture only".into(),
        session_id: resume.then(|| "saved-session".into()),
    };
    (dir, config)
}

#[cfg(unix)]
async fn collect(mode: &str, resume: bool) -> (RunOutcome, Vec<Event>) {
    let (_dir, config) = fixture(mode, resume);
    let (control_tx, control_rx) = mpsc::channel(8);
    let (events_tx, mut events_rx) = mpsc::channel(8);
    let task = tokio::spawn(run(config, control_rx, events_tx));
    let mut events = Vec::new();
    while let Some(event) = events_rx.recv().await {
        match &event {
            Event::Permission(question) if question.kind == "user_input" => {
                let answers = question
                    .questions
                    .iter()
                    .map(|q| {
                        (
                            q.id.clone(),
                            if q.id == "first" {
                                vec!["A".into(), "B".into()]
                            } else {
                                vec!["My custom answer".into()]
                            },
                        )
                    })
                    .collect();
                control_tx
                    .send(Control::UserInput {
                        request_id: question.request_id.clone(),
                        answers,
                    })
                    .await
                    .unwrap();
            }
            Event::Permission(question) => control_tx
                .send(Control::Permission {
                    request_id: question.request_id.clone(),
                    option_id: question.options[0].id.clone(),
                })
                .await
                .unwrap(),
            Event::Text { delta, .. } if delta == "cancel-ready" => {
                control_tx.send(Control::Cancel).await.unwrap()
            }
            _ => {}
        }
        events.push(event);
    }
    (task.await.unwrap().unwrap(), events)
}

#[cfg(unix)]
#[tokio::test]
async fn interleaved_stream_permissions_usage_and_unknown_reverse_requests_are_correlated() {
    let (outcome, events) = collect("normal", false).await;
    assert_eq!(outcome.status, "completed", "{:?}", outcome.error);
    assert_eq!(outcome.usage.input_tokens, Some(30));
    assert_eq!(outcome.usage.output_tokens, Some(4));
    assert_eq!(outcome.usage.cost_usd, Some(0.001));
    let texts: Vec<&str> = events
        .iter()
        .filter_map(|event| {
            if let Event::Text { role, delta } = event {
                (role == "assistant").then_some(delta.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(texts, ["Hello ", "Forge"]);
    assert!(events.iter().any(|event| matches!(event, Event::Permission(question) if question.request_id == "\"native-question\"" && question.options[0].id == "allow-42")));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Session(id) if id == "new-session"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn restoring_session_suppresses_replay_and_never_creates_a_replacement() {
    let (outcome, events) = collect("normal", true).await;
    assert_eq!(outcome.status, "completed", "{:?}", outcome.error);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Text{delta,..} if delta.contains("REPLAY")))
    );
    let (outcome, events) = collect("no-resume", true).await;
    assert_eq!(outcome.status, "failed");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Session(_)))
    );
    let (outcome, events) = collect("load-failure", true).await;
    assert_eq!(outcome.status, "failed");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Session(_)))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sign_in_failure_and_malformed_initialization_never_submit_a_prompt() {
    for mode in [
        "auth-failure",
        "wrong-id",
        "malformed",
        "oversized",
        "api-key-default",
    ] {
        let (outcome, events) = collect(mode, false).await;
        assert_eq!(outcome.status, "failed", "mode {mode}: {:?}", outcome.error);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Session(_))),
            "mode {mode}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_waits_for_protocol_acknowledgement() {
    let (outcome, _) = collect("cancel", false).await;
    assert_eq!(outcome.status, "cancelled", "{:?}", outcome.error);
}

#[cfg(unix)]
#[tokio::test]
async fn missing_cancellation_acknowledgement_requires_reconciliation() {
    let (outcome, _) = collect("cancel-no-ack", false).await;
    assert_eq!(outcome.status, "unknown");
    assert!(
        outcome
            .error
            .as_deref()
            .unwrap()
            .contains("did not acknowledge cancellation")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn process_exit_after_submission_is_unknown_instead_of_retried() {
    let (outcome, _) = collect("exit-during-prompt", false).await;
    assert_eq!(outcome.status, "unknown");
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_terminates_only_the_owned_process_group() {
    let mut unrelated = Command::new("/usr/bin/env")
        .args(["python3", "-c", "import sys; sys.stdin.read()"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (outcome, _) = collect("cancel", false).await;
    assert_eq!(outcome.status, "cancelled");
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().await.unwrap();
    unrelated.wait().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn same_group_descendant_remaining_after_leader_exit_keeps_run_unknown() {
    let (outcome, events) = collect("lingering-child", false).await;
    let pid: i32 = events
        .iter()
        .find_map(|event| match event {
            Event::Text { delta, .. } => delta
                .strip_prefix("child-pid:")
                .and_then(|s| s.parse().ok()),
            _ => None,
        })
        .unwrap();
    let still_alive = unsafe { libc::kill(pid, 0) } == 0;
    // This PID came from the fixture that spawned it, not a process-name search.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    assert!(still_alive);
    assert_eq!(outcome.status, "unknown");
    assert!(
        outcome
            .error
            .unwrap()
            .contains("could not be confirmed stopped")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn probe_only_asks_for_the_version_and_does_not_authenticate() {
    let (_dir, config) = fixture("normal", false);
    let result = probe(config.executable.to_str().unwrap()).await;
    assert!(result.available);
    assert_eq!(
        result.version.as_deref(),
        Some("grok fixture-1 (not a live provider)")
    );
    assert_eq!(result.auth_status, "not_checked");
}

#[cfg(unix)]
#[tokio::test]
async fn native_user_question_forms_preserve_ids_multiselect_and_free_text() {
    for mode in ["user-input", "user-input-wrapped"] {
        let (outcome, events) = collect(mode, false).await;
        assert_eq!(outcome.status, "completed", "{mode}: {:?}", outcome.error);
        let form = events
            .iter()
            .find_map(|event| match event {
                Event::Permission(q) if q.kind == "user_input" => Some(q),
                _ => None,
            })
            .unwrap();
        assert_eq!(form.request_id, "17");
        assert_eq!(form.questions.len(), 2);
        assert!(form.questions[0].multi_select);
        assert!(form.options.is_empty());
    }
}

#[test]
fn user_input_validation_rejects_mismatched_fields_and_ambiguous_native_questions() {
    let questions = vec![
        json!({"id":"first","question":"Pick one","options":[{"label":"A","preview":"A preview"},{"label":"B"}],"multiSelect":false}),
    ];
    assert!(
        normalize_user_answers(
            &questions,
            &HashMap::from([("wrong".into(), vec!["A".into()])])
        )
        .is_err()
    );
    assert!(
        normalize_user_answers(
            &questions,
            &HashMap::from([("first".into(), vec!["A".into(), "B".into()])])
        )
        .is_err()
    );
    assert!(parse_user_questions(&[questions[0].clone(), questions[0].clone()]).is_err());
    let result = normalize_user_answers(
        &questions,
        &HashMap::from([("first".into(), vec!["A".into(), "Clarification".into()])]),
    )
    .unwrap();
    assert_eq!(result["answers"]["Pick one"], json!(["A"]));
    assert_eq!(
        result["annotations"]["Pick one"],
        json!({"preview":"A preview","notes":"Clarification"})
    );
}
