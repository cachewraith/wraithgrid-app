//! Port of `tests/unit/slash.test.ts`: the `/` menu, the `$` skills menu, and the claude
//! adapter's commands and permission modes.

use serde_json::json;
use wraithgrid_chat::slash::*;
use wraithgrid_chat::*;

fn cmd(name: &str) -> ChatCommand {
    ChatCommand {
        name: name.into(),
        ..ChatCommand::default()
    }
}

/// Shapes from claude 2.1.306's initialize answer (`commands`) and init
/// (`terminal_slash_commands`).
fn commands() -> Vec<ChatCommand> {
    vec![
        ChatCommand {
            description: "(product-management) Brainstorm a product idea".into(),
            argument_hint: "<topic, problem, or idea to explore>".into(),
            ..cmd("product-management:brainstorm")
        },
        ChatCommand {
            description: "(postman) Use when testing an endpoint".into(),
            ..cmd("postman:api-testing")
        },
        ChatCommand {
            description: "(postman) Use when creating a stand-in backend".into(),
            ..cmd("postman:api-mocking")
        },
        ChatCommand {
            aliases: vec!["testing".into()],
            ..cmd("cachewraith-skills:testing")
        },
        ChatCommand {
            builtin: true,
            description: "Clear conversation history but keep a summary".into(),
            ..cmd("compact")
        },
        ChatCommand {
            builtin: true,
            ..cmd("clear")
        },
        ChatCommand {
            builtin: true,
            ..cmd("model")
        },
        ChatCommand {
            builtin: true,
            ..cmd("doctor")
        },
    ]
}

fn items() -> Vec<SlashItem> {
    build_slash_items(&commands(), &["doctor".into()])
}

#[test]
fn slash_query_is_the_first_word_after_a_leading_slash() {
    assert_eq!(slash_query("/", 1), Some(""));
    assert_eq!(slash_query("/api", 4), Some("api"));
    assert_eq!(slash_query("/api", 2), Some("a"));
    assert_eq!(slash_query("/api test", 9), None);
    assert_eq!(slash_query("hi /api", 7), None);
    assert_eq!(slash_query("/api", 0), None);
    // A caret inside a character is moved back to its start.
    assert_eq!(slash_query("/é", 2), Some(""));
}

#[test]
fn puts_ours_first_and_hides_the_cli_commands_ours_replace() {
    let items = items();
    let names: Vec<&str> = items[..app_commands().len()]
        .iter()
        .map(|i| i.name.as_str())
        .collect();
    assert_eq!(names, ["new", "model", "effort", "mode", "usage"]);
    assert!(!items
        .iter()
        .any(|i| i.name == "clear" || (i.name == "model" && i.group != SlashGroup::App)));
}

#[test]
fn groups_skills_and_builtins_and_leaves_out_terminal_only_commands() {
    let items = items();
    let by = |n: &str| items.iter().find(|i| i.name == n).cloned();
    let testing = by("postman:api-testing").unwrap();
    assert_eq!(testing.group, SlashGroup::Skill);
    assert_eq!(testing.source.as_deref(), Some("postman"));
    assert_eq!(testing.description, "Use when testing an endpoint");
    assert_eq!(by("compact").unwrap().group, SlashGroup::Builtin);
    // Chats have no terminal view (#110), so a command that only works there isn't offered.
    assert!(by("doctor").is_none());
    let order = |g: SlashGroup| match g {
        SlashGroup::App => 0,
        SlashGroup::Skill => 1,
        _ => 2,
    };
    let groups: Vec<u8> = items.iter().map(|i| order(i.group)).collect();
    let mut sorted = groups.clone();
    sorted.sort();
    assert_eq!(groups, sorted);
    assert_eq!(SlashGroup::Builtin.label(), "Built in");
}

fn names(q: &str) -> Vec<String> {
    rank_slash(&items(), q, SLASH_LIMIT)
        .into_iter()
        .map(|i| i.name)
        .collect()
}

#[test]
fn lists_everything_for_a_bare_slash() {
    assert_eq!(names("").len(), items().len());
}

#[test]
fn ranks_exact_then_prefix_then_the_part_after_the_plugin_then_aliases() {
    assert_eq!(names("usage")[0], "usage");
    assert_eq!(names("clear")[0], "new");
    assert_eq!(names("api-t")[0], "postman:api-testing");
    assert_eq!(
        names("postman"),
        ["postman:api-mocking", "postman:api-testing"]
    );
    assert_eq!(names("testing")[0], "cachewraith-skills:testing");
    assert_eq!(names("brain")[0], "product-management:brainstorm");
}

#[test]
fn matches_letters_in_order_then_descriptions_and_nothing_for_nonsense() {
    assert!(names("apmk").contains(&"postman:api-mocking".to_string()));
    assert_eq!(names("summary"), ["compact"]);
    assert!(names("zzzz").is_empty());
}

#[test]
fn splits_a_sent_command_from_its_arguments() {
    let lc = |name: &str, rest: &str| {
        Some(LeadingCommand {
            name: name.into(),
            rest: rest.into(),
        })
    };
    assert_eq!(
        leading_command("/postman:api-testing check login"),
        lc("postman:api-testing", "check login")
    );
    assert_eq!(leading_command("/compact"), lc("compact", ""));
    assert_eq!(leading_command("hello /x"), None);
    assert_eq!(leading_command("/not a/ path"), lc("not", "a/ path"));
    assert_eq!(leading_command("/x\nmore\nlines"), lc("x", "more\nlines"));
}

#[test]
fn claude_reads_the_initialize_commands_and_the_terminal_only_list() {
    let mut a = ClaudeAdapter::new();
    let id = a.hello()[0]["request_id"].clone();
    let caps = a
        .decode(&json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": id,
                "response": {
                    "models": [],
                    "commands": [
                        { "name": "postman:api-testing", "description": "(postman) Test", "argumentHint": "", "aliases": [] },
                        { "name": "compact", "description": "Compact", "argumentHint": "<instructions>", "builtin": true },
                        { "name": "__remote-workflow", "description": "internal" },
                        { "name": "bad name; rm", "description": "x" }
                    ]
                }
            }
        }))
        .events
        .remove(0);
    let ChatEvent::Capabilities {
        commands: Some(commands),
        ..
    } = caps
    else {
        panic!("not capabilities: {caps:?}")
    };
    assert_eq!(commands.len(), 2);
    assert_eq!(
        (commands[0].name.as_str(), commands[0].builtin),
        ("postman:api-testing", false)
    );
    assert!(commands[0].aliases.is_empty());
    assert_eq!(
        (
            commands[1].name.as_str(),
            commands[1].builtin,
            commands[1].argument_hint.as_str()
        ),
        ("compact", true, "<instructions>")
    );
    let session = a
        .decode(&json!({
            "type": "system", "subtype": "init", "session_id": "s1",
            "terminal_slash_commands": ["doctor", "color"]
        }))
        .events
        .remove(0);
    assert!(matches!(
        session,
        ChatEvent::Session { terminal_only: Some(t), .. } if t == ["doctor", "color"]
    ));
}

#[test]
fn claude_reads_the_mode_switches_it_live_and_starts_in_it() {
    let mut a = ClaudeAdapter::new();
    assert_eq!(
        a.decode(&json!({ "type": "system", "subtype": "status", "status": null, "permissionMode": "plan" }))
            .events,
        vec![ChatEvent::Mode {
            mode: ChatMode::Plan
        }]
    );
    assert!(a
        .decode(&json!({ "type": "system", "subtype": "status", "permissionMode": "weird" }))
        .events
        .is_empty());
    let req = a.set_mode(ChatMode::Auto).unwrap();
    assert_eq!(req["type"], "control_request");
    assert_eq!(
        req["request"],
        json!({ "subtype": "set_permission_mode", "mode": "auto" })
    );
    let mut args = |mode: Option<ChatMode>| {
        a.args(&ArgsOptions {
            mode,
            ..ArgsOptions::default()
        })
    };
    let plan = args(Some(ChatMode::Plan));
    assert!(plan.windows(2).any(|w| w == ["--permission-mode", "plan"]));
    assert!(!args(Some(ChatMode::Default)).contains(&"--permission-mode".to_string()));
    assert!(!args(None).contains(&"--permission-mode".to_string()));
}

#[test]
fn a_codex_skill_is_named_with_dollar_in_the_slash_menu_too() {
    let items = build_slash_items(
        &[ChatCommand {
            mention: true,
            ..cmd("review")
        }],
        &[],
    );
    let review = items.iter().find(|i| i.name == "review").unwrap();
    assert_eq!((review.group, review.sigil), (SlashGroup::Skill, '$'));
    assert_eq!(items.iter().find(|i| i.name == "new").unwrap().sigil, '/');
}

#[test]
fn mention_query_is_the_dollar_word_the_caret_ends() {
    let wq = |start, query: &str| {
        Some(WordQuery {
            start,
            query: query.into(),
        })
    };
    assert_eq!(mention_query("$", 1), wq(0, ""));
    assert_eq!(mention_query("use $rev", 8), wq(4, "rev"));
    assert_eq!(mention_query("use $rev now", 12), None);
    assert_eq!(mention_query("a$b", 3), None);
    assert_eq!(mention_query("plain", 5), None);
}
