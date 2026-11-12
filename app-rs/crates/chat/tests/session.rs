//! The `chat sessions` cases of `tests/unit/chat.test.ts` that need a process, run against a
//! small shell stand-in for `claude -p` speaking the shapes of tests/fixtures/claude-chat.ndjson.
#![cfg(unix)]

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use wraithgrid_chat::*;

/// "run it" asks to run a Bash tool and waits for the approval; "crash" dies with an error on
/// stderr; "mute" makes it ignore the next Stop; anything else is answered at once.
const FAKE: &str = r#"
field() { printf '%s' "$2" | sed -E "s/.*\"$1\":\"([^\"]*)\".*/\1/"; }
result() { printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"duration_ms":5,"total_cost_usd":0.001}'; }
mute=no
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      printf '%s\n' "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$(field request_id "$line")\",\"response\":{\"models\":[{\"value\":\"sonnet\"}],\"commands\":[]}}}"
      printf '%s\n' "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s1\",\"model\":\"cfg:$CLAUDE_CONFIG_DIR\",\"permissionMode\":\"default\"}" ;;
    *'"subtype":"interrupt"'*)
      if [ "$mute" = no ]; then
        printf '%s\n' '{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}'
        printf '%s\n' '{"type":"result","subtype":"error_during_execution","is_error":true}'
      fi ;;
    *'"behavior"'*)
      printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"denied","is_error":true}]}}'
      result ;;
    *'run it'*)
      printf '%s\n' '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ls"}}]}}'
      printf 'noise that is not json\n'
      printf '%s\n' '{"type":"control_request","request_id":"rq1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"permission_suggestions":[],"tool_use_id":"tu1"}}' ;;
    *'crash'*)
      printf 'warming up\nError: Invalid API key\n' >&2
      exit 1 ;;
    *'mute'*) mute=yes ;;
    *'slow'*) ;;
    *'"type":"user"'*)
      printf '%s\n' '{"type":"stream_event","event":{"type":"message_start","message":{"id":"m2"}}}'
      printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hel"}}}'
      printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}}'
      printf '%s\n' '{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"hello"}]}}'
      result ;;
  esac
done
"#;

fn launch() -> ChatLaunch {
    let mut l = ChatLaunch::new("sh", std::env::temp_dir());
    l.args = vec!["-c".into(), FAKE.into(), "fake-claude".into()];
    l.config_dir = Some("/accounts/work".into());
    l.stop_grace = Duration::from_millis(500);
    l
}

fn start(l: ChatLaunch) -> (ChatSession, Receiver<ChatEvent>) {
    ChatSession::start(l, chat_adapter_for("claude").unwrap()).expect("starts")
}

/// Events until `done` matches one (that one included), or panics after 5 s.
fn until(rx: &Receiver<ChatEvent>, done: impl Fn(&ChatEvent) -> bool) -> Vec<ChatEvent> {
    let end = Instant::now() + Duration::from_secs(5);
    let mut out = Vec::new();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let e = rx
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("timed out; got {out:?}"));
        let stop = done(&e);
        out.push(e);
        if stop {
            return out;
        }
    }
}

fn idle(e: &ChatEvent) -> bool {
    *e == ChatEvent::Busy { busy: false }
}

#[test]
fn says_hello_streams_a_turn_and_folds_into_a_thread() {
    let (chat, rx) = start(launch());
    let first = until(&rx, |e| matches!(e, ChatEvent::Session { .. }));
    assert!(first.iter().any(
        |e| matches!(e, ChatEvent::Capabilities { models, .. } if models[0].value == "sonnet")
    ));
    // The account's config dir went in as CLAUDE_CONFIG_DIR.
    assert!(matches!(
        first.last(),
        Some(ChatEvent::Session { id, model: Some(m), .. }) if id == "s1" && m == "cfg:/accounts/work"
    ));
    let mut t = ChatThread::new();
    for e in &first {
        t.update(e, 0);
    }
    t.add_user_message("hi", 0, 0);
    assert!(chat.send("hi", &[], &[]));
    for e in until(&rx, idle) {
        t.update(&e, 0);
    }
    assert!(!t.busy);
    let texts: Vec<&str> = t
        .items
        .iter()
        .filter_map(|i| match i {
            ChatItem::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    // Streamed once as deltas, not again from the full message.
    assert_eq!(texts, ["hello"]);
    assert!(t.items.iter().any(|i| matches!(
        i,
        ChatItem::Turn {
            cost_usd: Some(_),
            ..
        }
    )));
    assert!(chat.is_running());
    assert!(!chat.is_busy());
}

#[test]
fn answers_approvals_and_skips_noise() {
    let (chat, rx) = start(launch());
    assert!(chat.send("run it", &[], &[]));
    let got = until(&rx, |e| matches!(e, ChatEvent::Approval { .. }));
    assert_eq!(got[0], ChatEvent::Busy { busy: true });
    assert!(got
        .iter()
        .any(|e| matches!(e, ChatEvent::Tool { id, .. } if id == "tu1")));
    assert!(chat.is_busy());
    assert!(!chat.approve("nope", ApprovalDecision::Allow, None));
    assert!(chat.approve("rq1", ApprovalDecision::Deny, None));
    let rest = until(&rx, idle);
    assert_eq!(
        rest[0],
        ChatEvent::ApprovalDone {
            req_id: "rq1".into(),
            decision: ApprovalDecision::Deny,
            answers: None
        }
    );
    assert!(rest
        .iter()
        .any(|e| matches!(e, ChatEvent::ToolResult { is_error: true, .. })));
    // Answered once only.
    assert!(!chat.approve("rq1", ApprovalDecision::Allow, None));
}

#[test]
fn reports_a_crash_with_its_last_stderr_line() {
    let (chat, rx) = start(launch());
    chat.send("crash", &[], &[]);
    let got = until(&rx, |e| matches!(e, ChatEvent::Exit { .. }));
    assert_eq!(
        got[got.len() - 2..],
        [
            ChatEvent::Busy { busy: false },
            ChatEvent::Exit {
                code: Some(1),
                signal: None,
                error: Some("Error: Invalid API key".into())
            }
        ]
    );
    assert!(!chat.is_running());
    assert!(!chat.send("again", &[], &[]));
}

#[test]
fn a_requested_stop_is_quiet() {
    let (chat, rx) = start(launch());
    until(&rx, |e| matches!(e, ChatEvent::Session { .. }));
    chat.stop();
    assert!(chat.wait_exit(Duration::from_secs(5)));
    assert!(!chat.is_running());
    std::thread::sleep(Duration::from_millis(100));
    assert!(rx.try_iter().all(|e| !matches!(e, ChatEvent::Exit { .. })));
}

#[test]
fn interrupt_goes_over_the_pipe_and_ends_the_turn_quietly() {
    let (chat, rx) = start(launch());
    chat.send("slow", &[], &[]);
    chat.interrupt();
    let got = until(&rx, idle);
    assert!(got.contains(&ChatEvent::Notice {
        level: NoticeLevel::Info,
        text: "Interrupted".into()
    }));
    // A stopped turn is not an error.
    assert!(got
        .iter()
        .any(|e| matches!(e, ChatEvent::TurnEnd { error: None, .. })));
    assert!(chat.is_running());
}

#[test]
fn stop_with_no_answer_still_ends_the_wait() {
    let mut l = launch();
    l.interrupt_wait = Duration::from_millis(50);
    let (chat, rx) = start(l);
    chat.send("mute", &[], &[]);
    chat.interrupt();
    // claude says nothing back.
    let got = until(&rx, idle);
    assert!(!got.iter().any(|e| matches!(e, ChatEvent::TurnEnd { .. })));
    assert!(!chat.is_busy());
}

#[test]
fn a_start_that_cannot_run_says_why() {
    let mut l = launch();
    l.program = "/nonexistent/claude".into();
    assert!(matches!(
        ChatSession::start(l, Box::new(ClaudeAdapter::new())),
        Err(StartError::Spawn(_))
    ));
    let mut l = launch();
    l.options.session_id = Some("../etc".into());
    assert!(matches!(
        ChatSession::start(l, Box::new(ClaudeAdapter::new())),
        Err(StartError::Invalid(_))
    ));
    let mut l = launch();
    l.options.effort = Some("huge".into());
    assert!(ChatSession::start(l, Box::new(ClaudeAdapter::new())).is_err());
}

#[test]
fn a_message_past_the_limits_is_not_sent() {
    let (chat, _rx) = start(launch());
    let img = ChatImage {
        media_type: ChatImageType::Png,
        data: "not base64!".into(),
    };
    assert!(!chat.send("x", &[img], &[]));
    let files = vec!["f".to_string(); CHAT_FILES_MAX + 1];
    assert!(!chat.send("x", &[], &files));
    assert!(!chat.is_busy());
}
