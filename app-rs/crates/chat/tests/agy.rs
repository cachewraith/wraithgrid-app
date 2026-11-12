//! Port of `tests/unit/chat-agy.test.ts`: the agy adapter against real agy 1.4.4 output (a
//! conversation, and a command refused for want of an approval), its args, its model list and
//! its tool cards. (The `AGENT_CLI_SPECS` checks belong to the app, not here.)

use serde_json::{json, Value};
use wraithgrid_chat::agy::usage_of;
use wraithgrid_chat::thread::{ChatItem, ChatStatus, ChatThread};
use wraithgrid_chat::*;

/// Real agy 1.4.4 output (tool list and home path taken out).
fn load(name: &str) -> Vec<Value> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path)
        .expect("fixture")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect()
}

/// A command and an edit, then two more messages (the second sent while the first ran).
fn chat() -> Vec<Value> {
    load("agy-chat.ndjson")
}

/// A command that needed approval, refused because headless agy can't ask.
fn denied() -> Vec<Value> {
    load("agy-denied.ndjson")
}

fn decode_all(msgs: &[Value]) -> Vec<ChatEvent> {
    let mut a = AgyAdapter::new();
    msgs.iter().flat_map(|m| a.decode(m).events).collect()
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
    let got = serde_json::to_value(got).expect("serializes");
    assert!(matches(&got, &want), "{got:#}\ndoes not match\n{want:#}");
}

fn count(events: &[ChatEvent], f: impl Fn(&ChatEvent) -> bool) -> usize {
    events.iter().filter(|e| f(e)).count()
}

fn opts(model: Option<&str>, effort: Option<&str>) -> ArgsOptions {
    ArgsOptions {
        model: model.map(String::from),
        effort: effort.map(String::from),
        ..ArgsOptions::default()
    }
}

#[test]
fn records_each_turns_tokens_from_result_usage_never_a_cost() {
    let events = decode_all(&chat());
    let end = events
        .iter()
        .find(|e| matches!(e, ChatEvent::TurnEnd { .. }))
        .expect("turn end");
    // Real agy 1.4.4: input 4869, output 307, thinking 0, cache read 10995.
    assert_matches(
        end,
        json!({ "usage": { "costUsd": null, "input": 4869, "output": 307, "cacheRead": 10995, "cacheWrite": 0 } }),
    );
    assert_eq!(usage_of(&Value::Null, None), None);
    assert_eq!(
        usage_of(
            &json!({ "output_tokens": 5, "thinking_tokens": 7 }),
            Some("m")
        ),
        Some(TurnUsage {
            model: Some("m".into()),
            cost_usd: None,
            input: 0,
            output: 12,
            cache_read: 0,
            cache_write: 0,
        })
    );
}

#[test]
fn is_registered_and_lists_its_models_with_agy_models() {
    let a = chat_adapter_for("agy").expect("agy has a chat view");
    let list = a.list_models().expect("agy lists its models apart");
    assert_eq!(list.args, ["models"]);
    assert_eq!(a.config_dir_var(), None);
}

#[test]
fn runs_agy_headless_with_stream_json_resume_model_effort_and_mode_as_flags() {
    parse_agy_models("gemini-3.8-flash-low\tGemini 3.8 Flash (Low)\nclaude-sonnet-4-6\tClaude");
    let mut a = AgyAdapter::new();
    assert_eq!(
        a.args(&opts(None, None)),
        [
            "--print=",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json"
        ]
    );
    let resume = a.args(&ArgsOptions {
        session_id: Some("c-1".into()),
        ..opts(None, None)
    });
    assert_eq!(resume[resume.len() - 2..], ["--conversation", "c-1"]);
    let cont = a.args(&ArgsOptions {
        continue_last: true,
        ..opts(None, None)
    });
    assert_eq!(cont.last().map(String::as_str), Some("--continue"));
    assert!(a
        .args(&opts(Some("gemini-3.8-flash"), Some("low")))
        .contains(&"--effort".to_string()));
    // A model without levels refuses --effort: leave it out.
    assert!(!a
        .args(&opts(Some("claude-sonnet-4-6"), Some("low")))
        .contains(&"--effort".to_string()));
    let plan = a.args(&ArgsOptions {
        mode: Some(ChatMode::Plan),
        ..opts(None, None)
    });
    assert_eq!(plan[plan.len() - 2..], ["--mode", "plan"]);
    let full = a.args(&ArgsOptions {
        mode: Some(ChatMode::BypassPermissions),
        ..opts(None, None)
    });
    assert_eq!(
        full.last().map(String::as_str),
        Some("--dangerously-skip-permissions")
    );
    assert_eq!(
        a.user("hi", &[]),
        Some(json!({ "event": "user", "message": { "role": "user", "content": "hi" } }))
    );
    // Nothing of these goes over agy's pipe: the view restarts it instead.
    assert_eq!(a.interrupt(), None);
    assert_eq!(a.set_model("x"), None);
    assert_eq!(a.set_mode(ChatMode::Plan), None);
    assert_eq!(a.answer("r", ApprovalDecision::Allow, None), None);
}

#[test]
fn folds_the_per_level_model_ids_of_agy_models_into_one_model_with_efforts() {
    let models = parse_agy_models(
        &[
            "gemini-3.8-flash-high\tGemini 3.8 Flash (High)",
            "gemini-3.8-flash-medium\tGemini 3.8 Flash (Medium)",
            "gemini-3.8-flash-low\tGemini 3.8 Flash (Low)",
            "claude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)",
            "Fetching available models...",
            "",
        ]
        .join("\n"),
    );
    let model = |value: &str, label: &str, efforts: &[&str]| ChatModel {
        value: value.into(),
        label: label.into(),
        description: String::new(),
        efforts: efforts.iter().map(|e| e.to_string()).collect(),
        auto_mode: false,
    };
    assert_eq!(
        models,
        [
            model(
                "gemini-3.8-flash",
                "Gemini 3.8 Flash",
                &["low", "medium", "high"]
            ),
            model(
                "claude-opus-4-6-thinking",
                "Claude Opus 4.6 (Thinking)",
                &[]
            ),
        ]
    );
}

#[test]
fn decodes_a_real_conversation_session_controls_streamed_text_tools_turn_ends() {
    let events = decode_all(&chat());
    assert_eq!(
        events[0],
        ChatEvent::Session {
            id: "ad7ddba0-cf3f-47d8-a323-a79d3047ca0f".into(),
            model: Some("claude-sonnet-4-6".into()),
            terminal_only: None,
        }
    );
    assert_matches(
        &events[1],
        json!({
            "t": "controls",
            "controls": { "live": { "interrupt": false, "model": false, "mode": false }, "approvals": false }
        }),
    );
    let tools: Vec<&ChatEvent> = events
        .iter()
        .filter(|e| matches!(e, ChatEvent::Tool { .. }))
        .collect();
    assert_matches(
        tools[0],
        json!({ "name": "run_command", "kind": "shell", "subtitle": "echo wg-test > /tmp/agytest/b.txt" }),
    );
    assert!(tools.iter().any(|t| matches!(
        t,
        ChatEvent::Tool { name, kind: ToolKind::Edit, .. } if name == "write_file"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        ChatEvent::ToolResult { output, is_error: false, .. }
            if output == "Command exited with code 0.\n\nNo output from the command."
    )));
    // One result, and one turn end, per message.
    assert_eq!(
        count(&events, |e| matches!(e, ChatEvent::TurnEnd { .. })),
        3
    );
    assert_eq!(count(&events, |e| matches!(e, ChatEvent::Busy { .. })), 3);

    let mut t = ChatThread::new();
    t.add_user_message("go", 0, 1000);
    for e in &events {
        t.apply(e, 1000);
    }
    let text: String = t
        .items
        .iter()
        .filter_map(|it| match it {
            ChatItem::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("I'll run both tasks in parallel!"));
    assert!(text.contains("OK."));
    assert_eq!(t.status(), ChatStatus::Idle);
    assert_eq!(
        t.controls.map(|c| c.modes),
        Some(vec![
            ChatMode::Default,
            ChatMode::AcceptEdits,
            ChatMode::Plan,
            ChatMode::BypassPermissions
        ])
    );
}

#[test]
fn says_what_it_refused_for_want_of_an_approval_and_how_to_allow_it() {
    let notices: Vec<(NoticeLevel, String)> = decode_all(&denied())
        .into_iter()
        .filter_map(|e| match e {
            ChatEvent::Notice { level, text } => Some((level, text)),
            _ => None,
        })
        .collect();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, NoticeLevel::Error);
    assert!(notices[0].1.contains("skipped: RunCommand"));
    assert!(notices[0].1.contains("Full access"));
}

#[test]
fn a_result_answering_several_queued_messages_ends_each_of_their_turns() {
    let mut a = AgyAdapter::new();
    let step = json!({ "event": "step_update", "step_update": { "state": "DONE", "step_type": "user_input" } });
    a.decode(&step);
    a.decode(&step);
    let ev = a
        .decode(&json!({ "event": "result", "result": { "status": "SUCCESS" } }))
        .events;
    assert_eq!(count(&ev, |e| matches!(e, ChatEvent::Busy { .. })), 2);
}

#[test]
fn a_failed_turn_is_an_error_and_a_sign_in_failure_says_how_to_sign_in() {
    let mut a = AgyAdapter::new();
    let ev = a
        .decode(&json!({
            "event": "result",
            "result": { "status": "ERROR", "error": "Print mode: not authenticated" }
        }))
        .events;
    assert_matches(&ev[0], json!({ "t": "notice", "level": "error" }));
    assert!(matches!(&ev[0], ChatEvent::Notice { text, .. } if text.contains("not signed in")));
    assert!(ev.iter().any(|e| matches!(
        e,
        ChatEvent::TurnEnd { error: Some(err), .. } if err == "Print mode: not authenticated"
    )));
}

#[test]
fn draws_its_tools_by_kind() {
    assert_eq!(
        describe_agy_tool("view_file", &json!({ "Path": "/a" })),
        ToolDescription {
            kind: ToolKind::Read,
            subtitle: "/a".into(),
            preview: None,
        }
    );
    assert_eq!(
        describe_agy_tool("grep_search", &json!({ "Query": "foo" })).kind,
        ToolKind::Search
    );
    assert_eq!(
        describe_agy_tool("mystery", &json!({})).kind,
        ToolKind::Other
    );
}
