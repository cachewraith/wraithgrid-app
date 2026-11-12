//! Port of `tests/unit/chat.test.ts`: the claude adapter against real claude 2.1.306 output,
//! the thread reducer, the saved view, the turn bookkeeping and attachments. (The markdown
//! cases belong to the view and are not ported here.)

use serde_json::{json, Value};
use wraithgrid_chat::thread::{valid_thumbs, ChatItem, ChatStatus, ChatThread, ItemLevel};
use wraithgrid_chat::*;

fn lines(name: &str) -> Vec<Value> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path)
        .expect("fixture")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect()
}

/// Real claude 2.1.306 output (paths and the plugin list taken out): a Write that asks for
/// approval, an interrupted turn, then a second turn in the same process.
fn fixture() -> Vec<Value> {
    lines("claude-chat.ndjson")
}

/// Real claude 2.1.306 output: a Bash call with run_in_background, the turn ends, the
/// command finishes, and claude reads its result in a turn of its own (#39).
fn background() -> Vec<Value> {
    lines("claude-background.ndjson")
}

fn decode_all() -> Vec<ChatEvent> {
    let mut a = ClaudeAdapter::new();
    fixture().iter().flat_map(|m| a.decode(m).events).collect()
}

fn fold(events: &[ChatEvent], mut t: ChatThread) -> ChatThread {
    for e in events {
        t.apply(e, 1000);
    }
    t
}

fn opts() -> ArgsOptions {
    ArgsOptions::default()
}

fn to_json(v: &impl serde::Serialize) -> Value {
    serde_json::to_value(v).expect("serializes")
}

/// `toMatchObject`: every field of `want` is in `got`, recursively (arrays element-wise).
fn matches(got: &Value, want: &Value) -> bool {
    match (got, want) {
        (Value::Object(g), Value::Object(w)) => w
            .iter()
            .all(|(k, v)| g.get(k).is_some_and(|gv| matches(gv, v))),
        (Value::Array(g), Value::Array(w)) => {
            g.len() == w.len() && g.iter().zip(w).all(|(a, b)| matches(a, b))
        }
        _ => got == want,
    }
}

#[track_caller]
fn assert_matches(got: &impl serde::Serialize, want: Value) {
    let got = to_json(got);
    assert!(matches(&got, &want), "{got:#}\ndoes not match\n{want:#}");
}

fn hello_id(a: &mut ClaudeAdapter) -> String {
    a.hello()[0]["request_id"].as_str().unwrap().to_string()
}

// ---- claude chat adapter -----------------------------------------------------------------

#[test]
fn runs_claude_headless_with_stream_json_and_approvals_over_stdio() {
    let mut a = ClaudeAdapter::new();
    let args = a.args(&opts());
    assert_eq!(args[0], "-p");
    let joined = args.join(" ");
    assert!(joined.contains("--input-format stream-json --output-format stream-json"));
    assert!(joined.contains("--permission-prompt-tool stdio"));
    let resume = a.args(&ArgsOptions {
        session_id: Some("abc-1".into()),
        continue_last: true,
        ..opts()
    });
    assert_eq!(resume[resume.len() - 2..], ["--resume", "abc-1"]);
    let cont = a.args(&ArgsOptions {
        continue_last: true,
        ..opts()
    });
    assert_eq!(cont.last().unwrap(), "--continue");
    let hello = a.hello();
    assert_eq!(hello.len(), 1);
    assert_matches(
        &hello[0],
        json!({ "type": "control_request", "request": { "subtype": "initialize" } }),
    );
}

#[test]
fn reads_models_and_plan_from_the_initialize_answer_and_switches_model_live() {
    let mut a = ClaudeAdapter::new();
    let id = hello_id(&mut a);
    // The shape of real claude 2.1.306 output, trimmed.
    let out = a.decode(&json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": id,
            "response": {
                "models": [
                    {
                        "value": "sonnet",
                        "displayName": "Sonnet 5.5",
                        "description": "Most efficient for simpler tasks",
                        "supportsEffort": true,
                        "supportedEffortLevels": ["low", "high"]
                    },
                    { "value": "x", "displayName": "X", "supportsEffort": false },
                    { "displayName": "no value" }
                ],
                "account": { "email": "me@example.com", "subscriptionType": "Claude Pro" }
            }
        }
    }));
    assert_eq!(
        to_json(&out.events),
        json!([{
            "t": "capabilities",
            // This answer lists no commands.
            "commands": [],
            "models": [
                {
                    "value": "sonnet",
                    "label": "Sonnet 5.5",
                    "description": "Most efficient for simpler tasks",
                    "efforts": ["low", "high"],
                    "autoMode": false
                },
                { "value": "x", "label": "X", "description": "", "efforts": [], "autoMode": false }
            ],
            "account": { "email": "me@example.com", "plan": "Claude Pro" }
        }])
    );
    // Another request's answer is not the model list.
    let other = a.decode(&json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": "other" }
    }));
    assert!(other.events.is_empty());
    assert_matches(
        &a.set_model("haiku"),
        json!({ "type": "control_request", "request": { "subtype": "set_model", "model": "haiku" } }),
    );
    let args = a.args(&ArgsOptions {
        session_id: Some("s".into()),
        model: Some("opus".into()),
        effort: Some("max".into()),
        ..opts()
    });
    assert_eq!(
        args[args.len() - 6..],
        ["--resume", "s", "--model", "opus", "--effort", "max"]
    );
}

#[test]
fn allows_bypass_mid_chat_and_a_refused_mode_switch_keeps_the_mode() {
    let mut a = ClaudeAdapter::new();
    assert!(a
        .args(&opts())
        .contains(&"--allow-dangerously-skip-permissions".to_string()));
    a.decode(&json!({ "type": "system", "subtype": "status", "permissionMode": "acceptEdits" }));
    let req = a.set_mode(ChatMode::BypassPermissions).unwrap();
    // Real claude 2.1.306 answer for a session started without the allow flag.
    let out = a.decode(&json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": req["request_id"],
            "error": "Cannot set permission mode to bypassPermissions because the session was not launched with --dangerously-skip-permissions",
            "error_code": "bypass_not_launched"
        }
    }));
    assert_eq!(out.events.len(), 2);
    assert_matches(&out.events[0], json!({ "t": "notice", "level": "error" }));
    assert_eq!(
        out.events[1],
        ChatEvent::Mode {
            mode: ChatMode::AcceptEdits
        }
    );
    // Another request's error is not a mode change.
    let other = a.decode(&json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": "x" }
    }));
    assert!(other.events.is_empty());
}

#[test]
fn a_cli_without_an_adapter_has_no_chat_view() {
    assert!(chat_adapter_for("claude").is_some());
    assert!(chat_adapter_for("gemini").is_none());
}

#[test]
fn decodes_a_real_session() {
    let events = decode_all();
    assert_eq!(
        to_json(&events[0]),
        json!({
            "t": "session",
            "id": "dd6d7ff1-33ba-479a-93f6-7d4d4c475db9",
            "model": "claude-haiku-5-5",
            "terminalOnly": []
        })
    );
    let tool = events.iter().find(|e| matches!(e, ChatEvent::Tool { .. }));
    assert_matches(
        &tool,
        json!({
            "t": "tool",
            "id": "toolu_01QXxLZjFxVanbdFjNHLRbFZ",
            "name": "Write",
            "kind": "edit",
            "subtitle": "/work/hello.txt",
            "preview": { "type": "diff", "before": "", "after": "hi\n" }
        }),
    );
    let approval = events
        .iter()
        .find(|e| matches!(e, ChatEvent::Approval { .. }));
    assert_matches(
        &approval,
        json!({ "toolId": "toolu_01QXxLZjFxVanbdFjNHLRbFZ", "name": "Write", "canAlways": true }),
    );
    let result = events
        .iter()
        .find(|e| matches!(e, ChatEvent::ToolResult { .. }));
    assert_matches(&result, json!({ "isError": false }));
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Text { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    // Streamed once as deltas, not again from the full message.
    assert_eq!(text.matches("Created `hello.txt`").count(), 1);
    assert!(text.contains("ok"));
}

#[test]
fn a_stopped_turn_is_a_notice_not_an_error() {
    let events = decode_all();
    let errors: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TurnEnd { error, .. } => Some(error.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(errors, vec![None, None, None]);
    assert!(events.contains(&ChatEvent::Notice {
        level: NoticeLevel::Info,
        text: "Interrupted".into()
    }));
}

#[test]
fn answers_approvals_with_the_input_claude_asked_about() {
    let mut a = ClaudeAdapter::new();
    let req = fixture()
        .into_iter()
        .find(|m| m["type"] == "control_request")
        .unwrap();
    a.decode(&req);
    let id = req["request_id"].as_str().unwrap();
    assert_eq!(a.answer("nope", ApprovalDecision::Allow, None), None);
    assert_eq!(
        a.answer(id, ApprovalDecision::Always, None),
        Some(json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": id,
                "response": {
                    "behavior": "allow",
                    "updatedInput": { "file_path": "/work/hello.txt", "content": "hi\n" },
                    "updatedPermissions": [
                        { "type": "setMode", "mode": "acceptEdits", "destination": "session" }
                    ]
                }
            }
        }))
    );
    // Answered once only.
    assert_eq!(a.answer(id, ApprovalDecision::Deny, None), None);
}

#[test]
fn ask_user_question_shows_its_questions_and_sends_the_picks_back() {
    let mut a = ClaudeAdapter::new();
    let input = json!({
        "questions": [{
            "question": "Which color?",
            "header": "Color",
            "multiSelect": false,
            "options": [
                { "label": "Red", "description": "Warm" },
                { "label": "Blue", "description": "Cool" }
            ]
        }]
    });
    let out = a.decode(&json!({
        "type": "control_request",
        "request_id": "rq",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": "AskUserQuestion",
            "input": input,
            "permission_suggestions": [],
            "tool_use_id": "tq"
        }
    }));
    assert_matches(
        &out.events[0],
        json!({ "t": "approval", "questions": input["questions"] }),
    );
    let answers: ChatAnswers = [("Which color?".to_string(), "Blue".to_string())].into();
    let mut want_input = input.clone();
    want_input["answers"] = json!({ "Which color?": "Blue" });
    assert_matches(
        &a.answer("rq", ApprovalDecision::Allow, Some(&answers)),
        json!({ "response": { "response": { "behavior": "allow", "updatedInput": want_input } } }),
    );
}

#[test]
fn refuses_control_requests_meant_for_an_sdk_host() {
    let out = ClaudeAdapter::new().decode(&json!({
        "type": "control_request",
        "request_id": "r1",
        "request": { "subtype": "hook_callback" }
    }));
    assert!(out.events.is_empty());
    assert_eq!(
        out.replies,
        vec![json!({
            "type": "control_response",
            "response": { "subtype": "error", "request_id": "r1", "error": "Not supported by Wraithgrid" }
        })]
    );
}

#[test]
fn skips_a_subagents_own_steps() {
    let out = ClaudeAdapter::new().decode(&json!({
        "type": "assistant",
        "parent_tool_use_id": "toolu_x",
        "message": { "id": "m", "content": [{ "type": "text", "text": "inner" }] }
    }));
    assert!(out.events.is_empty());
}

#[test]
fn a_line_that_is_not_json_means_nothing() {
    let mut a = ClaudeAdapter::new();
    assert_eq!(a.decode_line("not json"), Decoded::default());
    assert_eq!(a.decode_line("   "), Decoded::default());
    let d = a.decode_line(" {\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\r");
    assert_matches(
        &d.events,
        json!([{ "t": "session", "id": "s", "model": null }]),
    );
}

#[test]
fn describes_tools_by_kind() {
    let bash = describe_claude_tool(
        "Bash",
        &json!({ "command": "ls -la\necho", "description": "List" }),
    );
    assert_eq!(bash.kind, ToolKind::Shell);
    assert_eq!(bash.subtitle, "List");
    assert_eq!(
        bash.preview,
        Some(ToolPreview::Code {
            text: "ls -la\necho".into()
        })
    );
    assert_eq!(
        describe_claude_tool("Grep", &json!({ "pattern": "foo", "path": "src" })).subtitle,
        "foo  in  src"
    );
    let todo = describe_claude_tool(
        "TodoWrite",
        &json!({ "todos": [
            { "content": "a", "status": "completed" },
            { "content": "b", "status": "in_progress" }
        ] }),
    );
    assert_eq!(
        (todo.kind, todo.subtitle.as_str()),
        (ToolKind::Todo, "1/2 done")
    );
    let other = describe_claude_tool("mcp__x__y", &json!({ "q": "hello" }));
    assert_eq!(
        (other.kind, other.subtitle.as_str()),
        (ToolKind::Other, "hello")
    );
    // No input at all shows nothing; a big one is clipped.
    assert_eq!(
        describe_claude_tool("mcp__x__y", &Value::Null).preview,
        None
    );
    assert_eq!(describe_claude_tool("mcp__x__y", &json!({})).preview, None);
}

#[test]
fn sends_pasted_images_as_base64_blocks_before_the_text() {
    let img = ChatImage {
        media_type: ChatImageType::Png,
        data: "AAAA".into(),
    };
    assert_eq!(
        ClaudeAdapter::new().user("look", &[img]),
        Some(json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } },
                    { "type": "text", "text": "look" }
                ]
            }
        }))
    );
    assert_eq!(
        ClaudeAdapter::new().user("hi", &[]),
        Some(json!({ "type": "user", "message": { "role": "user", "content": "hi" } }))
    );
}

#[test]
fn a_turn_end_reports_usage_and_limits() {
    let mut a = ClaudeAdapter::with_clock(Box::new(|| 5000));
    let out = a.decode(&json!({
        "type": "rate_limit_event",
        "rate_limit_info": { "unifiedWindows": {
            "five_hour": { "utilization": 0.42, "resetsAt": 100 },
            "seven_day": { "utilization": "x" }
        } }
    }));
    assert_eq!(
        out.events,
        vec![ChatEvent::Limits {
            limits: AccountLimits {
                five_hour: Some(UsageWindow {
                    utilization: 0.42,
                    resets_at: 100_000.0
                }),
                seven_day: None,
                at: 5000
            }
        }]
    );
    let out = a.decode(&json!({
        "type": "result", "subtype": "error_during_execution", "is_error": true,
        "duration_ms": 1200, "total_cost_usd": 0.0042,
        "usage": { "input_tokens": 12, "output_tokens": 340, "cache_read_input_tokens": 9000 },
        "modelUsage": { "claude-opus-5-5": {} }
    }));
    assert_eq!(
        out.events,
        vec![
            ChatEvent::TurnEnd {
                cost_usd: Some(0.0042),
                duration_ms: Some(1200.0),
                error: Some("The turn failed (error_during_execution).".into()),
                usage: Some(TurnUsage {
                    model: Some("claude-opus-5-5".into()),
                    cost_usd: Some(0.0042),
                    input: 12,
                    output: 340,
                    cache_read: 9000,
                    cache_write: 0
                })
            },
            ChatEvent::Busy { busy: false }
        ]
    );
}

// ---- background work (#39) ---------------------------------------------------------------

#[test]
fn lists_the_running_task_after_the_turn_ends_then_runs_claudes_own_turn() {
    let mut a = ClaudeAdapter::new();
    let all: Vec<ChatEvent> = background()
        .iter()
        .flat_map(|m| a.decode(m).events)
        .collect();
    let bg: Vec<&Vec<ChatBackgroundTask>> = all
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Background { tasks } => Some(tasks),
            _ => None,
        })
        .collect();
    assert_eq!(bg.len(), 2);
    assert_eq!(bg[0].len(), 1);
    assert_eq!(bg[0][0].id, "b8khg48uw");
    assert_eq!(bg[0][0].kind, BackgroundKind::Shell);
    assert_eq!(bg[0][0].label, "Sleep 15 seconds then echo done");
    assert!(bg[1].is_empty());
    // The ending is news, under the tool that started it (#63), and claude's unasked
    // follow-up turn marks the chat busy again.
    let at = all
        .iter()
        .position(|e| matches!(e, ChatEvent::TaskResult { .. }))
        .unwrap();
    assert_eq!(
        all[at],
        ChatEvent::TaskResult {
            tool_id: "toolu_019GrbvoSNixgnNjkvrCAZ5W".into(),
            ok: true,
            text: "Background command \"Sleep 15 seconds then echo done\" completed (exit code 0)"
                .into()
        }
    );
    assert!(all[at..].contains(&ChatEvent::Busy { busy: true }));

    // In the thread: idle but "background" between the two turns, idle at the end.
    let mut t = ChatThread::new();
    t.add_user_message("run it", 0, 1000);
    let first_end = all
        .iter()
        .position(|e| matches!(e, ChatEvent::TurnEnd { .. }))
        .unwrap();
    t = fold(&all[..first_end + 2], t);
    assert!(!t.busy);
    assert_eq!(t.status(), ChatStatus::Background);
    t = fold(&all[first_end + 2..], t);
    assert!(t.background.is_empty());
    assert_eq!(t.status(), ChatStatus::Idle);
    let tool = t
        .items
        .iter()
        .find(|i| i.id() == "toolu_019GrbvoSNixgnNjkvrCAZ5W");
    match tool {
        Some(ChatItem::Tool(tool)) => {
            let r = tool.result.as_ref().unwrap();
            assert!(r.ok && r.text.contains("completed"));
        }
        other => panic!("no tool: {other:?}"),
    }
    assert!(!t
        .items
        .iter()
        .any(|i| matches!(i, ChatItem::Notice { text, .. } if text.contains("completed"))));
}

#[test]
fn a_result_for_a_tool_not_in_the_thread_falls_back_to_a_short_notice() {
    let mut t = ChatThread::new();
    t.apply(
        &ChatEvent::TaskResult {
            tool_id: "gone".into(),
            ok: false,
            text: "x".repeat(500),
        },
        0,
    );
    assert_eq!(t.items.len(), 1);
    assert!(matches!(
        &t.items[0],
        ChatItem::Notice { level: ItemLevel::Error, text, .. } if *text == "x".repeat(300)
    ));
}

#[test]
fn a_saved_result_is_kept_only_when_well_formed() {
    let load = |result: Value| {
        let raw = json!({ "v": 1, "items": [{
            "kind": "tool", "id": "t1", "name": "Agent", "toolKind": "agent", "subtitle": "s",
            "preview": null, "output": "ok", "isError": false, "approval": null, "result": result
        }] });
        ChatThread::parse(Some(&raw.to_string())).items.remove(0)
    };
    let ok = load(json!({ "ok": true, "text": "**Verdict:** PASS" }));
    assert_matches(
        &ok,
        json!({ "result": { "ok": true, "text": "**Verdict:** PASS" } }),
    );
    // Round-trips through the saved view.
    let t = ChatThread {
        items: vec![ok],
        ..ChatThread::default()
    };
    assert_eq!(ChatThread::parse(Some(&t.serialize())).items, t.items);
    let bad = to_json(&load(json!({ "ok": "yes", "text": 1 })));
    assert!(bad.get("result").is_none());
}

#[test]
fn keeps_a_tasks_start_time_and_the_process_ending_clears_the_list() {
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(1000));
    let c = clock.clone();
    let mut a = ClaudeAdapter::with_clock(Box::new(move || {
        c.load(std::sync::atomic::Ordering::SeqCst)
    }));
    let mut list = |ids: &[&str]| {
        let tasks: Vec<Value> = ids
            .iter()
            .map(|id| json!({ "task_id": id, "task_type": "local_agent", "description": id }))
            .collect();
        a.decode(
            &json!({ "type": "system", "subtype": "background_tasks_changed", "tasks": tasks }),
        )
        .events
        .remove(0)
    };
    let first = list(&["x"]);
    clock.store(9000, std::sync::atomic::Ordering::SeqCst);
    let second = list(&["x", "y"]);
    let ChatEvent::Background { tasks } = &second else {
        panic!("not a list")
    };
    let ChatEvent::Background { tasks: before } = &first else {
        panic!("not a list")
    };
    assert_eq!(tasks[0].since, before[0].since);
    assert_eq!(tasks[1].since, 9000);
    assert_eq!(tasks[1].kind, BackgroundKind::Agent);
    let t = fold(
        &[
            second.clone(),
            ChatEvent::Exit {
                code: Some(0),
                signal: None,
                error: None,
            },
        ],
        ChatThread::new(),
    );
    assert!(t.background.is_empty());
}

#[test]
fn a_long_foreground_command_is_a_task_too_but_its_end_is_no_notice() {
    // Real claude 2.1.306 output for `sleep 5 && echo hi` run in the foreground, trimmed.
    let mut a = ClaudeAdapter::new();
    let events: Vec<ChatEvent> = [
        json!({ "type": "system", "subtype": "task_started", "task_id": "bjnrnx53m",
                "tool_use_id": "toolu_01", "description": "Wait then say hi",
                "is_backgrounded": false, "task_type": "local_bash" }),
        json!({ "type": "system", "subtype": "task_notification", "task_id": "bjnrnx53m",
                "tool_use_id": "toolu_01", "status": "completed", "output_file": "",
                "summary": "Wait then say hi" }),
    ]
    .iter()
    .flat_map(|m| a.decode(m).events)
    .collect();
    assert!(events.is_empty());
}

// ---- chat thread -------------------------------------------------------------------------

fn bash_tool(id: &str, subtitle: &str) -> ChatEvent {
    ChatEvent::Tool {
        id: id.into(),
        name: "Bash".into(),
        kind: ToolKind::Shell,
        subtitle: subtitle.into(),
        preview: None,
    }
}

fn approval(req: &str, tool: &str) -> ChatEvent {
    ChatEvent::Approval {
        req_id: req.into(),
        tool_id: Some(tool.into()),
        name: "Bash".into(),
        subtitle: "ls".into(),
        preview: None,
        can_always: false,
        questions: None,
    }
}

#[test]
fn folds_the_real_session_into_messages_a_tool_card_and_turn_footers() {
    let mut t = ChatThread::new();
    t.add_user_message("make hello.txt", 0, 1000);
    assert_eq!(t.status(), ChatStatus::Running);
    let t = fold(&decode_all(), t);
    assert_eq!(
        t.session_id.as_deref(),
        Some("66c48029-202a-4285-83f3-e703fccd20e8")
    );
    assert!(!t.busy);
    assert!(matches!(t.items[0], ChatItem::User { .. }));
    let turns = t
        .items
        .iter()
        .filter(|i| matches!(i, ChatItem::Turn { .. }))
        .count();
    assert_eq!(turns, 3);
    let Some(ChatItem::Tool(tool)) = t.items.iter().find(|i| matches!(i, ChatItem::Tool(_))) else {
        panic!("no tool card")
    };
    assert!(tool.output.as_deref().unwrap().contains("File created"));
    assert!(!tool.is_error);
    // Streamed text is one growing item per block.
    assert!(t.items.iter().any(
        |i| matches!(i, ChatItem::Assistant { text, .. } if text.contains("Created `hello.txt`"))
    ));
}

#[test]
fn a_pending_approval_makes_the_status_approval_until_answered() {
    let mut t = ChatThread::new();
    t.apply(&ChatEvent::Busy { busy: true }, 0);
    let mut t = fold(&[bash_tool("x", "ls"), approval("r", "x")], t);
    assert_eq!(t.items.len(), 1);
    assert_eq!(t.pending_approval().map(|p| p.id.as_str()), Some("x"));
    assert_eq!(t.status(), ChatStatus::Approval);
    t.apply(
        &ChatEvent::ApprovalDone {
            req_id: "r".into(),
            decision: ApprovalDecision::Allow,
            answers: None,
        },
        0,
    );
    assert_eq!(t.status(), ChatStatus::Running);
}

#[test]
fn a_turn_ending_settles_an_unanswered_approval() {
    let t = fold(
        &[
            ChatEvent::Busy { busy: true },
            bash_tool("x", "ls"),
            approval("r", "x"),
            ChatEvent::Busy { busy: false },
        ],
        ChatThread::new(),
    );
    assert!(t.pending_approval().is_none());
    assert_eq!(t.turn_started_at, None);
}

#[test]
fn a_crash_shows_its_error_and_ends_the_turn() {
    let t = fold(
        &[
            ChatEvent::Busy { busy: true },
            ChatEvent::Exit {
                code: Some(1),
                signal: None,
                error: Some("Not logged in".into()),
            },
        ],
        ChatThread::new(),
    );
    assert!(!t.busy);
    assert!(matches!(
        t.items.last(),
        Some(ChatItem::Notice { level: ItemLevel::Error, text, .. }) if text == "Not logged in"
    ));
}

#[test]
fn round_trips_through_the_saved_view_with_approvals_settled() {
    let t = fold(
        &[
            ChatEvent::Session {
                id: "s1".into(),
                model: Some("m".into()),
                terminal_only: None,
            },
            bash_tool("x", "ls"),
            approval("r", "x"),
        ],
        ChatThread::new(),
    );
    let back = ChatThread::parse(Some(&t.serialize()));
    assert_eq!(back.session_id.as_deref(), Some("s1"));
    assert_eq!(back.restored, 1);
    assert!(back.pending_approval().is_none());
    assert_eq!(
        ChatThread::parse(Some(r#"{"v":99}"#)),
        ChatThread::default()
    );
    assert_eq!(ChatThread::parse(Some("not json")), ChatThread::default());
    assert_eq!(ChatThread::parse(None), ChatThread::default());
}

#[test]
fn the_saved_view_reads_the_ts_shape() {
    // A view as the Electron app saved it.
    let raw = r#"{"v":1,"sessionId":"s","model":null,"seq":7,"models":[],"items":[
        {"kind":"user","id":"u-1","text":"hi","images":0},
        {"kind":"assistant","id":"a","text":"**yes**"},
        {"kind":"turn","id":"t-2","costUsd":0.01,"durationMs":1200,"error":null},
        {"kind":"handoff","id":"h-3","text":"x","messages":2,"fromCli":"codex","toCli":"claude","divider":"d"},
        {"kind":"handoff","id":"h-4","text":"x","messages":2,"fromCli":"nope","toCli":"claude","divider":"d"},
        {"kind":"weird","id":"w"},
        {"kind":"notice","id":7,"level":"info","text":"no string id"}
    ]}"#;
    let t = ChatThread::parse(Some(raw));
    assert_eq!(t.seq, 7);
    let ids: Vec<&str> = t.items.iter().map(ChatItem::id).collect();
    assert_eq!(ids, ["u-1", "a", "t-2", "h-3"]);
    let saved: Value = serde_json::from_str(&t.serialize()).unwrap();
    assert_eq!(
        saved["items"][0],
        json!({ "kind": "user", "id": "u-1", "text": "hi", "images": 0 })
    );
}

#[test]
fn keeps_sent_thumbnails_in_the_saved_view_and_only_image_data_urls() {
    let png = "data:image/png;base64,iVBORw0KGgo=";
    let mut t = ChatThread::new();
    let id = t.add_user_message("look", 2, 1000);
    assert!(matches!(
        &t.items[0],
        ChatItem::User {
            images: 2,
            thumbs: None,
            ..
        }
    ));
    assert!(t.set_user_thumbs(
        &id,
        vec![png.into(), "data:image/webp;base64,UklGRg==".into()]
    ));
    assert!(!t.set_user_thumbs("nope", vec![png.into()]));
    let back = ChatThread::parse(Some(&t.serialize()));
    assert_eq!(back.items[0], t.items[0]);

    assert_eq!(
        valid_thumbs(&json!([
            png,
            "javascript:alert(1)",
            "data:text/html;base64,PGI+",
            "data:image/svg+xml;base64,PHN2Zz4=",
            7
        ])),
        Some(vec![png.to_string()])
    );
    assert_eq!(
        valid_thumbs(&json!(["data:image/png;base64,abc\"onerror=\"x"])),
        None
    );
    assert_eq!(valid_thumbs(&json!("x")), None);
    // A damaged save loses the thumbnails, not the message.
    let mut raw: Value = serde_json::from_str(&t.serialize()).unwrap();
    raw["items"][0]["thumbs"] = json!(["data:text/html;base64,PGI+"]);
    let bad = ChatThread::parse(Some(&raw.to_string()));
    assert!(matches!(
        &bad.items[0],
        ChatItem::User { text, images: 2, thumbs: None, .. } if text == "look"
    ));
}

#[test]
fn drops_saved_notices_that_only_repeat_the_tools_description() {
    let t = fold(
        &[
            bash_tool("x", "Run tests"),
            ChatEvent::Notice {
                level: NoticeLevel::Info,
                text: "Run tests".into(),
            },
            ChatEvent::Notice {
                level: NoticeLevel::Info,
                text: "Interrupted".into(),
            },
            bash_tool("y", "Build"),
            ChatEvent::Notice {
                level: NoticeLevel::Error,
                text: "Build".into(),
            },
        ],
        ChatThread::new(),
    );
    let back = ChatThread::parse(Some(&t.serialize()));
    let shown: Vec<&str> = back
        .items
        .iter()
        .map(|i| match i {
            ChatItem::Notice { text, .. } => text.as_str(),
            other => other.id(),
        })
        .collect();
    assert_eq!(shown, ["x", "Interrupted", "y", "Build"]);
}

#[test]
fn the_saved_view_goes_before_what_arrived_live() {
    let mut saved = ChatThread::new();
    saved.session_id = Some("old".into());
    saved.add_notice(ItemLevel::Info, "saved");
    let saved = ChatThread::parse(Some(&saved.serialize()));
    let mut live = ChatThread::new();
    live.apply(&ChatEvent::Busy { busy: true }, 5);
    live.add_notice(ItemLevel::Divider, "live");
    live.merge_saved(saved);
    assert_eq!(live.restored, 1);
    assert_eq!(live.seq, 2);
    assert_eq!(live.session_id.as_deref(), Some("old"));
    let ids: Vec<&str> = live.items.iter().map(ChatItem::id).collect();
    assert_eq!(ids, ["n-1", "n-1"]);
    live.settle("Stopped", 9);
    assert!(!live.busy);
    assert!(matches!(live.items.last(), Some(ChatItem::Notice { text, .. }) if text == "Stopped"));
}

// ---- live item cap (#141) ----------------------------------------------------------------

#[test]
fn a_long_chat_drops_its_oldest_items_in_one_step() {
    let items: Vec<ChatItem> = (0..1001)
        .map(|i| ChatItem::Notice {
            id: format!("n{i}"),
            level: ItemLevel::Info,
            text: i.to_string(),
        })
        .collect();
    let mut capped = ChatThread {
        items: items.clone(),
        restored: 300,
        ..ChatThread::default()
    };
    capped.cap_items();
    assert_eq!(capped.items.len(), 800);
    assert_eq!(capped.items[0].id(), "n201");
    assert_eq!(capped.restored, 99);
    let mut small = ChatThread {
        items: items[..1000].to_vec(),
        ..ChatThread::default()
    };
    small.cap_items();
    assert_eq!(small.items.len(), 1000);
}

// ---- turn bookkeeping (the TS `chat sessions` cases that need no process) ----------------

fn result_line(is_error: bool) -> Value {
    if is_error {
        json!({ "type": "result", "subtype": "success", "is_error": true,
                "result": "You've hit your session limit · resets 4:40pm" })
    } else {
        json!({ "type": "result", "subtype": "success", "is_error": false })
    }
}

/// Feeds lines through the adapter and the turn bookkeeping, as the session does.
fn feed(a: &mut ClaudeAdapter, t: &mut TurnState, out: &mut Vec<ChatEvent>, msg: &Value) {
    out.extend(t.filter(a.decode(msg).events));
}

fn busy_flags(events: &[ChatEvent]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Busy { busy } => Some(*busy),
            _ => None,
        })
        .collect()
}

#[test]
fn a_message_queued_while_a_turn_runs_keeps_the_chat_busy() {
    let (mut a, mut t, mut ev) = (ClaudeAdapter::new(), TurnState::default(), Vec::new());
    ev.push(t.sent());
    ev.push(t.sent());
    feed(&mut a, &mut t, &mut ev, &result_line(false));
    // The first turn ended, the queued one runs: still busy.
    assert_eq!(busy_flags(&ev), [true, true]);
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, ChatEvent::TurnEnd { .. }))
            .count(),
        1
    );
    feed(&mut a, &mut t, &mut ev, &result_line(false));
    assert_eq!(busy_flags(&ev), [true, true, false]);
    // An interrupt ends everything at once, queued turns included.
    ev.push(t.sent());
    ev.push(t.sent());
    t.interrupt();
    feed(&mut a, &mut t, &mut ev, &result_line(false));
    assert_eq!(busy_flags(&ev).last(), Some(&false));
    assert!(!t.is_busy());
}

#[test]
fn a_failed_turn_ends_the_queued_ones_too() {
    let (mut a, mut t, mut ev) = (ClaudeAdapter::new(), TurnState::default(), Vec::new());
    for _ in 0..3 {
        ev.push(t.sent());
    }
    feed(&mut a, &mut t, &mut ev, &result_line(true));
    assert_eq!(busy_flags(&ev).last(), Some(&false));
    assert_matches(
        &ev.iter().find(|e| matches!(e, ChatEvent::TurnEnd { .. })),
        json!({ "error": "You've hit your session limit · resets 4:40pm" }),
    );
}

#[test]
fn stop_with_no_turn_running_in_claude_still_ends_the_wait() {
    let mut t = TurnState::default();
    t.sent();
    let gen = t.interrupt();
    // claude says nothing back.
    assert_eq!(t.stop_waited(gen), Some(ChatEvent::Busy { busy: false }));
    assert!(!t.is_busy());
    // A turn end that came first cancels the wait.
    let mut a = ClaudeAdapter::new();
    t.sent();
    let gen = t.interrupt();
    t.filter(a.decode(&result_line(false)).events);
    assert_eq!(t.stop_waited(gen), None);
}

#[test]
fn a_turn_claude_starts_on_its_own_marks_the_chat_busy_once_then_idle() {
    let (mut a, mut t, mut ev) = (ClaudeAdapter::new(), TurnState::default(), Vec::new());
    ev.push(t.sent());
    for m in background() {
        feed(&mut a, &mut t, &mut ev, &m);
    }
    assert_eq!(busy_flags(&ev), [true, false, true, false]);
}

// ---- attachments -------------------------------------------------------------------------

#[test]
fn lists_attached_files_after_the_message() {
    assert_eq!(with_attachments("look", &[]), "look");
    assert_eq!(
        with_attachments("look", &["/a/b.ts".into(), "/c d.md".into()]),
        "look\n\nAttached files:\n- /a/b.ts\n- /c d.md"
    );
    assert_eq!(
        with_attachments("", &["/x\nevil".into()]),
        "Attached files:\n- /x evil"
    );
}

#[test]
fn reads_picked_images_as_images_and_other_files_as_paths() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let png = |n: u8| {
        let f = dir.path().join(format!("i{n}.PNG"));
        std::fs::write(&f, [0x89, 0x50, 0x4e, 0x47, n]).unwrap();
        f
    };
    let imgs: Vec<_> = (0..=CHAT_IMAGES_MAX as u8).map(png).collect();
    let txt = dir.path().join("notes.ts");
    std::fs::write(&txt, "x").unwrap();
    let mut picked = imgs.clone();
    picked.push(txt.clone());
    let r = attach::read_picked(&picked);
    assert_eq!(r.images.len(), CHAT_IMAGES_MAX);
    assert_eq!(
        r.images[0],
        ChatImage {
            media_type: ChatImageType::Png,
            data: base64::engine::general_purpose::STANDARD.encode([0x89, 0x50, 0x4e, 0x47, 0])
        }
    );
    let path = |p: &std::path::Path| p.to_string_lossy().into_owned();
    assert_eq!(r.files, vec![path(imgs.last().unwrap()), path(&txt)]);
    let many = attach::read_picked(&vec![txt.clone(); CHAT_FILES_MAX + 5]);
    assert_eq!(many.files.len(), CHAT_FILES_MAX);
}

#[test]
fn saves_pasted_images_for_clis_that_read_files() {
    let dir = tempfile::tempdir().unwrap();
    let img = ChatImage {
        media_type: ChatImageType::Jpeg,
        data: "AAEC".into(),
    };
    let files = attach::save_chat_images(&[img], dir.path(), 42).unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0].to_string_lossy().ends_with(".jpg"));
    assert_eq!(std::fs::read(&files[0]).unwrap(), [0, 1, 2]);
    assert!(attach::save_chat_images(&[], dir.path(), 42)
        .unwrap()
        .is_empty());
}

#[test]
fn chat_types_read_and_write_the_ts_names() {
    assert_eq!(
        cycle_modes(false),
        [ChatMode::Default, ChatMode::AcceptEdits, ChatMode::Plan]
    );
    assert_eq!(cycle_modes(true).last(), Some(&ChatMode::Auto));
    assert!(is_valid_model("claude-opus-5-5[1m]"));
    assert!(!is_valid_model("a b"));
    assert!(is_valid_session_id("bb740a2b-b0b4-422a-b18f-89ef6a25b391"));
    assert!(!is_valid_session_id("../x"));
    assert_eq!(clip("abcdef", 3), "abc\n… (3 more characters)");
    let e: ChatEvent =
        serde_json::from_value(json!({ "t": "approvalDone", "reqId": "r", "decision": "always" }))
            .unwrap();
    assert_eq!(
        e,
        ChatEvent::ApprovalDone {
            req_id: "r".into(),
            decision: ApprovalDecision::Always,
            answers: None
        }
    );
}
