//! Port of `tests/unit/chat-codex.test.ts`: the codex adapter against a real `codex app-server`
//! recording, plus the JSON-RPC edges (held messages, early stop, lost threads, approvals) and
//! the model and skill lists. (The `AGENT_CLI_SPECS` checks belong to the app, not here.)

use serde_json::{json, Value};
use wraithgrid_chat::thread::{ChatItem, ChatThread};
use wraithgrid_chat::*;

/// Real `codex app-server` output (codex-cli 0.163.0; account details replaced): a turn with
/// two commands that asked for approval, a turn interrupted mid-answer, then a short turn.
fn fixture() -> Vec<Value> {
    let path = format!(
        "{}/tests/fixtures/codex-chat.ndjson",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path)
        .expect("fixture")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect()
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

fn base() -> ArgsOptions {
    ArgsOptions::default()
}

/// Replays the recording, sending what the recorder sent at the same points.
fn replay() -> (Vec<Value>, Vec<ChatEvent>) {
    let mut a = CodexAdapter::new();
    a.args(&ArgsOptions {
        effort: Some("low".into()),
        ..base()
    });
    let mut sent = a.hello();
    let mut events = Vec::new();
    let mut turns = 0;
    let mut interrupted = false;
    for msg in fixture() {
        let out = a.decode(&msg);
        sent.extend(out.replies);
        for e in out.events {
            match &e {
                ChatEvent::Session { .. } => sent.push(a.user("do it", &[]).expect("turn/start")),
                ChatEvent::Approval { req_id, .. } => sent.push(
                    a.answer(req_id, ApprovalDecision::Allow, None)
                        .expect("pending"),
                ),
                ChatEvent::TurnEnd { .. } => {
                    turns += 1;
                    if turns < 3 {
                        sent.push(a.user("next", &[]).expect("turn/start"));
                    }
                }
                ChatEvent::Text { .. } if turns == 1 && !interrupted => {
                    interrupted = true;
                    sent.push(a.interrupt().expect("turn/interrupt"));
                }
                _ => {}
            }
            events.push(e);
        }
    }
    (sent, events)
}

/// The `id` of the request for `method` among `lines`.
fn request<'a>(lines: &'a [Value], method: &str) -> &'a Value {
    lines
        .iter()
        .find(|m| m["method"] == method)
        .unwrap_or_else(|| panic!("no {method} request"))
}

/// The adapter after `initialize` was answered, with those replies.
fn initialized(o: ArgsOptions) -> (CodexAdapter, Vec<Value>) {
    let mut a = CodexAdapter::new();
    a.args(&o);
    a.hello();
    let replies = a.decode(&json!({ "id": 1, "result": {} })).replies;
    (a, replies)
}

#[test]
fn counts_each_turn_as_the_change_in_the_thread_total_never_a_cost() {
    let (_, events) = replay();
    let used: Vec<Option<TurnUsage>> = events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TurnEnd { usage, .. } => Some(usage.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(used.len(), 3);
    // Real codex-cli 0.163.0 counts: turn 1 made two model calls; turn 2 none (repeats the
    // total); turn 3 one. `input` here excludes the cached part, which is cacheRead.
    assert_eq!(
        used[0],
        Some(TurnUsage {
            model: None,
            cost_usd: None,
            input: 28771 - 26368,
            output: 78,
            cache_read: 26368,
            cache_write: 0,
        })
    );
    assert_matches(&used[1], json!({ "input": 0, "output": 0, "cacheRead": 0 }));
    let sum: u64 = used
        .iter()
        .flatten()
        .map(|u| u.input + u.output + u.cache_read)
        .sum();
    assert_eq!(sum, 44916);
}

#[test]
fn a_resumed_thread_counts_from_its_first_report_not_its_whole_history() {
    let mut a = CodexAdapter::new();
    let tok = |n: u64| json!({ "inputTokens": n, "cachedInputTokens": 0, "outputTokens": 1 });
    a.decode(&json!({
        "method": "thread/tokenUsage/updated",
        "params": { "tokenUsage": { "total": tok(90_000), "last": tok(500) } }
    }));
    let events = a
        .decode(&json!({ "method": "turn/completed", "params": { "turn": { "id": "t", "status": "completed" } } }))
        .events;
    let end = events
        .iter()
        .find(|e| matches!(e, ChatEvent::TurnEnd { .. }))
        .expect("turn end");
    assert_matches(end, json!({ "usage": { "input": 500, "output": 1 } }));
}

#[test]
fn is_registered_and_runs_the_app_server() {
    assert!(chat_adapter_for("codex").is_some());
    let mut a = chat_adapter_for("codex").unwrap();
    assert_eq!(a.args(&base()), ["app-server"]);
    assert_eq!(a.config_dir_var(), Some("CODEX_HOME"));
    assert!(a.list_models().is_none());
}

#[test]
fn speaks_json_rpc_like_the_recording() {
    let (sent, _) = replay();
    let names: Vec<String> = sent
        .iter()
        .map(|m| match m["method"].as_str() {
            Some(method) => method.to_string(),
            None => format!("answer {}", m["id"]),
        })
        .collect();
    assert_eq!(
        names,
        [
            "initialize",
            "initialized",
            "thread/start",
            "model/list",
            "account/read",
            "account/rateLimits/read",
            "skills/list",
            "turn/start",
            "answer 0",
            "answer 1",
            "turn/start",
            "turn/interrupt",
            "turn/start",
        ]
    );
    assert_matches(
        &sent[2]["params"],
        json!({ "approvalPolicy": "untrusted", "sandbox": "workspace-write" }),
    );
    assert_matches(
        &sent[7]["params"],
        json!({
            "threadId": "01a16411-1120-7012-99ec-710e87e01fa5",
            "input": [{ "type": "text", "text": "do it", "text_elements": [] }],
            "effort": "low",
            "approvalPolicy": "untrusted"
        }),
    );
    assert_eq!(
        sent[8],
        json!({ "jsonrpc": "2.0", "id": 0, "result": { "decision": "accept" } })
    );
    assert_matches(
        &sent[11]["params"],
        json!({ "turnId": "01a16411-3802-7cc1-89d6-31ba12ef7f39" }),
    );
}

#[test]
fn turns_the_recording_into_chat_events() {
    let (_, events) = replay();
    assert_eq!(
        events
            .iter()
            .find(|e| matches!(e, ChatEvent::Session { .. })),
        Some(&ChatEvent::Session {
            id: "01a16411-1120-7012-99ec-710e87e01fa5".into(),
            model: Some("gpt-6.1-sol".into()),
            terminal_only: None,
        })
    );
    let caps: Vec<(&Vec<ChatModel>, &Option<ChatAccountInfo>)> = events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Capabilities {
                models, account, ..
            } => Some((models, account)),
            _ => None,
        })
        .collect();
    let first = caps.iter().find(|c| !c.0.is_empty()).expect("models");
    assert_matches(
        &first.0[0],
        json!({
            "value": "gpt-6.1-sol",
            "label": "GPT-6.1-Sol",
            "efforts": ["low", "medium", "high", "xhigh", "max"]
        }),
    );
    // The last one has both halves.
    let last = caps.last().expect("capabilities");
    assert_eq!(last.0.len(), 7);
    assert_eq!(
        last.1,
        &Some(ChatAccountInfo {
            email: Some("me@example.com".into()),
            plan: Some("ChatGPT team".into()),
        })
    );
    let limits = events
        .iter()
        .find(|e| matches!(e, ChatEvent::Limits { .. }))
        .expect("limits");
    assert_matches(
        limits,
        json!({ "limits": {
            "fiveHour": { "utilization": 0.0, "resetsAt": 1792692210000.0 },
            "sevenDay": { "utilization": 0.72 }
        } }),
    );
    let approval = events
        .iter()
        .find(|e| matches!(e, ChatEvent::Approval { .. }))
        .expect("approval");
    assert_matches(
        approval,
        json!({ "name": "Shell", "subtitle": "echo hi > c.txt", "canAlways": true }),
    );
    assert!(events.contains(&ChatEvent::Notice {
        level: NoticeLevel::Info,
        text: "Interrupted".into()
    }));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ChatEvent::TurnEnd { .. }))
            .count(),
        3
    );
    let busy: Vec<&ChatEvent> = events
        .iter()
        .filter(|e| matches!(e, ChatEvent::Busy { .. }))
        .collect();
    assert_eq!(busy, vec![&ChatEvent::Busy { busy: false }; 3]);

    let mut t = ChatThread::new();
    t.add_user_message("do it", 0, 1000);
    for e in &events {
        t.apply(e, 1000);
    }
    let tools: Vec<_> = t
        .items
        .iter()
        .filter_map(|it| match it {
            ChatItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].subtitle, "echo hi > c.txt");
    assert_eq!(tools[0].output.as_deref(), Some("Exit code 0"));
    assert!(!tools[0].is_error);
    let text: Vec<&str> = t
        .items
        .iter()
        .filter_map(|it| match it {
            ChatItem::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains(&"done"), "{text:?}");
    assert!(text.concat().contains("update notes.txt"));
}

#[test]
fn holds_a_message_until_the_thread_is_ready_and_an_early_stop_until_the_turn_starts() {
    let mut a = CodexAdapter::new();
    a.args(&base());
    a.hello();
    assert_eq!(a.user("early", &[]), None);
    let ready = a.decode(&json!({ "id": 1, "result": {} })).replies;
    let start = request(&ready, "thread/start");
    let replies = a
        .decode(&json!({ "id": start["id"], "result": { "thread": { "id": "t1" }, "model": "m" } }))
        .replies;
    let methods: Vec<&Value> = replies.iter().map(|m| &m["method"]).collect();
    assert_eq!(methods, [&json!("turn/start")]);
    assert_eq!(a.interrupt(), None);
    let on = a
        .decode(&json!({ "method": "turn/started", "params": { "turn": { "id": "u1" } } }))
        .replies;
    assert_matches(
        &on[0],
        json!({ "method": "turn/interrupt", "params": { "threadId": "t1", "turnId": "u1" } }),
    );
}

#[test]
fn resumes_by_thread_id_and_starts_anew_when_codex_no_longer_has_it() {
    let (mut a, ready) = initialized(ArgsOptions {
        session_id: Some("old".into()),
        ..base()
    });
    let resume = request(&ready, "thread/resume");
    assert_matches(&resume["params"], json!({ "threadId": "old" }));
    let out = a.decode(&json!({ "id": resume["id"], "error": { "message": "no rollout found" } }));
    assert_matches(&out.events[0], json!({ "t": "notice", "level": "info" }));
    assert_eq!(out.replies[0]["method"], "thread/start");
}

#[test]
fn continues_the_last_thread_or_starts_one() {
    let (mut a, ready) = initialized(ArgsOptions {
        continue_last: true,
        ..base()
    });
    let list = request(&ready, "thread/list");
    assert_eq!(list["params"], json!({ "limit": 1, "cwd": null }));
    let out = a.decode(&json!({ "id": list["id"], "result": { "data": [{ "id": "last" }] } }));
    assert_matches(
        &out.replies[0],
        json!({ "method": "thread/resume", "params": { "threadId": "last" } }),
    );
    let (mut b, ready) = initialized(ArgsOptions {
        continue_last: true,
        ..base()
    });
    let list = request(&ready, "thread/list");
    let out = b.decode(&json!({ "id": list["id"], "result": { "data": [] } }));
    assert_eq!(out.replies[0]["method"], "thread/start");
}

#[test]
fn mode_and_model_go_with_the_next_turn_and_the_answers_codex_wants() {
    let (mut a, ready) = initialized(base());
    let start = request(&ready, "thread/start");
    a.decode(&json!({ "id": start["id"], "result": { "thread": { "id": "t1" } } }));
    assert_eq!(a.set_mode(ChatMode::BypassPermissions), None);
    assert_eq!(a.set_model("gpt-mini"), None);
    assert_matches(
        &a.user("x", &[]).unwrap()["params"],
        json!({
            "model": "gpt-mini",
            "approvalPolicy": "never",
            "sandboxPolicy": { "type": "dangerFullAccess" }
        }),
    );
    a.decode(&json!({
        "id": 7,
        "method": "item/fileChange/requestApproval",
        "params": { "itemId": "f1", "reason": "write" }
    }));
    assert_eq!(
        a.answer("cx-7", ApprovalDecision::Always, None),
        Some(json!({ "jsonrpc": "2.0", "id": 7, "result": { "decision": "acceptForSession" } }))
    );
    a.decode(&json!({ "id": 8, "method": "execCommandApproval", "params": { "command": ["ls"] } }));
    assert_eq!(
        a.answer("cx-8", ApprovalDecision::Deny, None),
        Some(json!({ "jsonrpc": "2.0", "id": 8, "result": { "decision": "denied" } }))
    );
    // Answered once: not pending any more.
    assert_eq!(a.answer("cx-8", ApprovalDecision::Deny, None), None);
    // Requests for a richer host get a refusal, so codex doesn't wait for ever.
    let out = a.decode(&json!({ "id": 9, "method": "item/tool/requestUserInput", "params": {} }));
    assert_matches(
        &out.replies[0],
        json!({ "id": 9, "error": { "code": -32601 } }),
    );
}

#[test]
fn a_resolved_request_takes_its_approval_bar_away() {
    let mut a = CodexAdapter::new();
    let ev = a
        .decode(&json!({
            "id": 4,
            "method": "item/commandExecution/requestApproval",
            "params": { "itemId": "c1", "command": "ls" }
        }))
        .events;
    assert_matches(
        &ev[0],
        json!({ "t": "approval", "reqId": "cx-4", "toolId": "c1", "subtitle": "ls" }),
    );
    let done = a
        .decode(&json!({ "method": "serverRequest/resolved", "params": { "requestId": 4 } }))
        .events;
    assert_eq!(
        done,
        [ChatEvent::ApprovalDone {
            req_id: "cx-4".into(),
            decision: ApprovalDecision::Deny,
            answers: None,
        }]
    );
    assert_eq!(a.answer("cx-4", ApprovalDecision::Allow, None), None);
}

#[test]
fn draws_edits_with_their_diff_and_hides_hidden_models() {
    let d = describe_codex_item(&json!({
        "type": "fileChange",
        "changes": [{ "path": "a.ts", "kind": { "type": "update" }, "diff": "-a\n+b" }]
    }))
    .expect("a tool");
    assert_eq!(d.name, "Edit");
    assert_eq!(d.kind, ToolKind::Edit);
    assert_eq!(d.subtitle, "a.ts");
    assert_eq!(
        d.preview,
        Some(ToolPreview::Code {
            text: "-a\n+b".into()
        })
    );
    let models = parse_codex_models(&json!({
        "data": [{ "id": "x", "hidden": true }, { "id": "y", "displayName": "Y" }]
    }));
    let values: Vec<&str> = models.iter().map(|m| m.value.as_str()).collect();
    assert_eq!(values, ["y"]);
}

#[test]
fn lists_codex_skills_for_the_dollar_menu_and_sends_the_named_ones_as_skill_inputs() {
    // Shape from codex-cli 0.164.0's `skills/list` schema.
    let listed = json!({
        "data": [{
            "cwd": "/p",
            "errors": [],
            "skills": [
                {
                    "name": "review",
                    "path": "/h/skills/review/SKILL.md",
                    "description": "Long text",
                    "shortDescription": "Review the diff",
                    "enabled": true,
                    "scope": "user"
                },
                {
                    "name": "off",
                    "path": "/h/skills/off/SKILL.md",
                    "description": "",
                    "enabled": false,
                    "scope": "user"
                },
                {
                    "name": "review",
                    "path": "/p/.codex/skills/review/SKILL.md",
                    "description": "dup",
                    "enabled": true,
                    "scope": "repo"
                }
            ]
        }]
    });
    assert_eq!(
        parse_codex_skills(&listed),
        [CodexSkill {
            name: "review".into(),
            path: "/h/skills/review/SKILL.md".into(),
            description: "Review the diff".into(),
        }]
    );

    let (mut a, ready) = initialized(base());
    let list = request(&ready, "skills/list");
    let ev = a
        .decode(&json!({ "id": list["id"], "result": listed }))
        .events;
    assert_matches(
        &ev[0],
        json!({
            "t": "capabilities",
            "commands": [{ "name": "review", "description": "Review the diff", "mention": true }]
        }),
    );
    let start = request(&ready, "thread/start");
    a.decode(&json!({ "id": start["id"], "result": { "thread": { "id": "t1" } } }));
    assert_matches(
        &a.user("$review the diff, not $5", &[]).unwrap()["params"],
        json!({
            "input": [
                { "type": "text", "text": "$review the diff, not $5" },
                { "type": "skill", "name": "review", "path": "/h/skills/review/SKILL.md" }
            ]
        }),
    );
    assert_matches(
        &a.user("no skill here", &[]).unwrap()["params"],
        json!({ "input": [{ "type": "text" }] }),
    );
    // A skill file changed: ask again, past codex's cache.
    let again = a
        .decode(&json!({ "method": "skills/changed", "params": {} }))
        .replies;
    assert_matches(
        &again[0],
        json!({ "method": "skills/list", "params": { "forceReload": true } }),
    );
}

#[test]
fn says_how_to_sign_in_and_drops_quiet_refusals() {
    let (mut a, ready) = initialized(base());
    let account = request(&ready, "account/read");
    let ev = a
        .decode(&json!({ "id": account["id"], "result": { "account": null, "requiresOpenaiAuth": true } }))
        .events;
    assert_matches(
        &ev,
        json!([{ "t": "notice", "level": "error", "text": "codex is not signed in for this account. Use Login on the Accounts page." }]),
    );
    // The model list, the limits and the skills are extras: their refusal says nothing.
    let models = request(&ready, "model/list");
    let out = a.decode(&json!({ "id": models["id"], "error": { "message": "nope" } }));
    assert!(out.events.is_empty());
}
