//! The persisted config (`config.json`): types, validation and repair, ported from
//! `src/shared/schema.ts` and `src/shared/types.ts`. Both apps read the same file during the
//! move to GPUI, so the rules are zod's to the letter: a field with `.default` may be missing,
//! a field with `.catch` falls back when it is bad, anything else bad rejects the whole file
//! (the store then backs it up and starts from defaults). Unlike zod, unknown keys are kept, so
//! a field a newer Electron build adds survives a round trip through this app. Fields are
//! declared in schema order so the file is written in the same order as before.

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use crate::agent_cli::AgentCli;
use crate::types::*;

pub const CONFIG_VERSION: i64 = 1;

/// Unknown keys of an object, kept as they were.
pub type Extra = Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: i64,
    /// Override for the claude binary. Empty means auto-detect from PATH.
    pub claude_path: String,
    pub accounts: Vec<Account>,
    pub account_folders: Vec<AccountFolder>,
    pub workspaces: Vec<Workspace>,
    pub machines: Vec<Machine>,
    pub active_workspace: String,
    pub recent_folders: Vec<String>,
    pub settings: Settings,
    /// Scheduled chats (#77).
    pub schedules: Vec<ScheduledJob>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub name: String,
    /// Which CLI its panes run.
    pub cli: AgentCli,
    /// Passed to the CLI in its config-dir variable; may start with `~`. Never read by us.
    pub config_dir: String,
    pub color: String,
    /// Wraithgrid's own flag, never derived from files inside configDir.
    pub signed_in: bool,
    pub imported: bool,
    pub icon: String,
    /// The sidebar folder it sits in, or None for the top level.
    pub folder_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A sidebar folder that groups accounts. One level deep.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountFolder {
    pub id: String,
    pub name: String,
    pub collapsed: bool,
    pub icon: String,
    /// Icon tint, '#rrggbb', or '' for neutral.
    pub color: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pane {
    pub id: String,
    /// None only for plain shell panes.
    pub account_id: Option<String>,
    pub cwd: String,
    pub args: Vec<String>,
    pub shell: bool,
    pub title: String,
    /// Continue the pane's conversation when Wraithgrid restores it.
    pub resume: bool,
    /// Something was sent to the agent, so there is a conversation to continue.
    pub has_conversation: bool,
    pub view: PaneView,
    /// The chat view's conversation id, resumed when the pane starts again.
    pub chat_session_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<ChatEffort>,
    pub chat_mode: Option<ChatMode>,
    /// The project team agent it was started as (#79).
    pub team_agent_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// One agent of a project's team (#79): started as a chat titled with its role.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamAgent {
    pub id: String,
    pub role: String,
    pub account_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<ChatEffort>,
    pub chat_mode: Option<ChatMode>,
    /// Sent as its first message when it starts; '' = none.
    pub prompt: String,
    /// `worktree` (its own branch) or `shared` (the project's folder).
    pub folder: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LayoutNode {
    #[serde(rename_all = "camelCase")]
    Pane { pane_id: String },
    #[serde(rename_all = "camelCase")]
    Empty { slot_id: String },
    Split {
        dir: SplitDir,
        #[serde(serialize_with = "js_numbers")]
        sizes: Vec<f64>,
        children: Vec<LayoutNode>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Preview {
    pub command: String,
    pub url: String,
}

/// A closed chat, kept so it can be reopened and carry on.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedChat {
    pub pane: Pane,
    pub closed_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub color: String,
    /// Every pane in the workspace. Panes missing from `layout` are hidden, not closed.
    pub panes: Vec<Pane>,
    pub layout: Option<LayoutNode>,
    /// The machine its panes run on (H5); None: this computer.
    pub machine_id: Option<String>,
    /// The project's folder: where its new chats start. '' = none picked.
    pub folder: String,
    pub preview: Preview,
    pub account_id: Option<String>,
    /// Chats closed here, newest first.
    pub history: Vec<ArchivedChat>,
    pub team: Vec<TeamAgent>,
    /// New panes start on a branch of their own, a git worktree (#162).
    pub worktree_panes: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Another computer whose Wraithgrid runs this workspace's panes, reached with `ssh` (H5).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub ssh_host: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub font_family: TerminalFont,
    pub font_size: i64,
    #[serde(serialize_with = "js_number")]
    pub ui_zoom: f64,
    pub theme: ThemePreference,
    pub theme_dark: String,
    pub theme_light: String,
    pub accent: Accent,
    pub terminal_palette: TerminalPalette,
    pub default_account_id: Option<String>,
    pub default_cwd: String,
    pub pinned_folders: Vec<String>,
    pub sidebar_collapsed: bool,
    pub sidebar_open_projects: Vec<String>,
    pub sidebar_width: i64,
    pub diff_width: i64,
    pub shared_mode: SharedMode,
    pub check_updates_on_launch: bool,
    pub notify_panes: bool,
    pub auto_switch_mode: AutoSwitchMode,
    pub auto_switch_at: i64,
    pub shell_path: String,
    pub shell_args: Vec<String>,
    pub mod_dirs: Vec<String>,
    pub gpu_terminal: bool,
    pub on_window_close: CloseMode,
    pub cli_access: bool,
    pub new_pane_view: PaneView,
    pub chat_only_migrated: bool,
    pub chat_model: Option<String>,
    pub chat_effort: Option<ChatEffort>,
    pub chat_mode: Option<ChatMode>,
    #[serde(serialize_with = "ordered_map")]
    pub cli_paths: Vec<(AgentCli, String)>,
    #[serde(serialize_with = "ordered_map")]
    pub shortcuts: Vec<(ShortcutId, String)>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            font_family: TerminalFont::JetBrainsMono,
            font_size: FONT_SIZE_DEFAULT,
            ui_zoom: 1.0,
            theme: ThemePreference::Dark,
            theme_dark: "wraith".into(),
            theme_light: "daylight".into(),
            accent: Accent::Theme,
            terminal_palette: TerminalPalette::Match,
            default_account_id: None,
            default_cwd: "~".into(),
            pinned_folders: vec![],
            sidebar_collapsed: false,
            sidebar_open_projects: vec![],
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            diff_width: DIFF_WIDTH_DEFAULT,
            shared_mode: SharedMode::Overall,
            check_updates_on_launch: true,
            notify_panes: true,
            auto_switch_mode: AutoSwitchMode::Ask,
            auto_switch_at: 95,
            shell_path: String::new(),
            shell_args: vec![],
            mod_dirs: vec![],
            gpu_terminal: false,
            on_window_close: CloseMode::Ask,
            cli_access: false,
            new_pane_view: PaneView::Chat,
            chat_only_migrated: false,
            chat_model: None,
            chat_effort: None,
            chat_mode: None,
            cli_paths: vec![],
            shortcuts: vec![],
            extra: Extra::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScheduleWhen {
    Once {
        at: String,
    },
    Weekly {
        days: Vec<i64>,
        time: String,
    },
    Hours {
        every: i64,
        from: String,
    },
    /// `fiveHour` or `sevenDay`.
    Reset {
        window: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScheduleTarget {
    New,
    #[serde(rename_all = "camelCase")]
    Chat {
        pane_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRun {
    pub at: i64,
    /// running, done, failed, missed, skipped or waiting.
    pub result: String,
    pub pane_id: Option<String>,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledJob {
    pub id: String,
    pub ws_id: String,
    pub account_id: String,
    pub prompt: String,
    pub model: Option<String>,
    pub effort: Option<ChatEffort>,
    pub chat_mode: Option<ChatMode>,
    pub when: ScheduleWhen,
    pub target: ScheduleTarget,
    /// `wait` or `switch`.
    pub at_limit: String,
    pub enabled: bool,
    pub one_shot: bool,
    pub next_at: Option<i64>,
    pub runs: Vec<ScheduleRun>,
}

// ---- Serialization helpers: write numbers and maps the way JSON.stringify does ----------

/// `1` rather than `1.0` for a whole number, as JavaScript writes it.
fn js_number<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.fract() == 0.0 && v.abs() < 9.007_199_254_740_992e15 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

fn js_numbers<S: Serializer>(v: &[f64], s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(v.len()))?;
    for n in v {
        seq.serialize_element(&JsNum(*n))?;
    }
    seq.end()
}

struct JsNum(f64);
impl Serialize for JsNum {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        js_number(&self.0, s)
    }
}

fn ordered_map<K: Serialize, S: Serializer>(v: &[(K, String)], s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut map = s.serialize_map(Some(v.len()))?;
    for (k, val) in v {
        map.serialize_entry(k, val)?;
    }
    map.end()
}

// ---- Value checks shared by the schema ---------------------------------------------------

/// zod measures string length in UTF-16 code units.
fn len16(s: &str) -> usize {
    s.encode_utf16().count()
}

fn chars_in(s: &str, extra: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

pub fn is_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && chars_in(s, "_-")
}

fn is_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// `CHAT_MODEL_PATTERN`: /^[A-Za-z0-9._[\]-]{1,100}$/
pub fn is_chat_model(s: &str) -> bool {
    (1..=100).contains(&s.len()) && chars_in(s, "._[]-")
}

/// `chatSessionIdSchema`: /^[A-Za-z0-9_-]{1,128}$/
pub fn is_chat_session_id(s: &str) -> bool {
    (1..=128).contains(&s.len()) && chars_in(s, "_-")
}

/// Never starts with `-` (it would be read as an ssh option) and has no spaces or quotes.
pub fn is_ssh_host(s: &str) -> bool {
    let ok = |c: char| c.is_ascii_alphanumeric() || "_.@%:[]".contains(c);
    (1..=255).contains(&s.len())
        && s.chars().next().is_some_and(ok)
        && s.chars().skip(1).all(|c| ok(c) || c == '-')
}

fn is_hhmm(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && matches!(
            (b[0], b[1]),
            (b'0'..=b'1', b'0'..=b'9') | (b'2', b'0'..=b'3')
        )
        && (b'0'..=b'5').contains(&b[3])
        && b[4].is_ascii_digit()
}

/// `LOCAL_DATETIME_PATTERN`: YYYY-MM-DDTHH:MM
fn is_local_datetime(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 16
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'T'
        && is_hhmm(&s[11..])
}

fn as_int(v: &Value) -> Option<i64> {
    let f = v.as_f64()?;
    (f.fract() == 0.0).then_some(f as i64)
}

/// A `.catch` field: anything missing or invalid becomes the fallback.
fn catch<T>(v: Option<&Value>, fallback: T, parse: impl FnOnce(&Value) -> Option<T>) -> T {
    v.and_then(parse).unwrap_or(fallback)
}

fn nullable<T>(v: &Value, parse: impl FnOnce(&Value) -> Option<T>) -> Option<Option<T>> {
    if v.is_null() {
        Some(None)
    } else {
        parse(v).map(Some)
    }
}

fn str_where(v: &Value, ok: impl FnOnce(&str) -> bool) -> Option<String> {
    v.as_str().filter(|s| ok(s)).map(String::from)
}

fn enum_of<T>(v: &Value, parse: impl FnOnce(&str) -> Option<T>) -> Option<T> {
    v.as_str().and_then(parse)
}

/// An object being validated, with its path for error messages.
struct Obj<'a> {
    map: &'a Map<String, Value>,
    at: String,
}

type Res<T> = Result<T, String>;

impl<'a> Obj<'a> {
    fn new(v: &'a Value, at: impl Into<String>) -> Res<Self> {
        let at = at.into();
        match v.as_object() {
            Some(map) => Ok(Obj { map, at }),
            None => Err(format!("{at}: expected an object")),
        }
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key)
    }

    fn fail<T>(&self, key: &str, what: &str) -> Res<T> {
        Err(format!("{}.{key}: {what}", self.at))
    }

    /// Required, checked by `parse`.
    fn req<T>(&self, key: &str, what: &str, parse: impl FnOnce(&Value) -> Option<T>) -> Res<T> {
        match self.get(key).and_then(parse) {
            Some(v) => Ok(v),
            None => self.fail(key, what),
        }
    }

    /// `.default(d)`: missing takes `d`, present must be valid.
    fn or<T>(
        &self,
        key: &str,
        d: T,
        what: &str,
        parse: impl FnOnce(&Value) -> Option<T>,
    ) -> Res<T> {
        match self.get(key) {
            None => Ok(d),
            Some(v) => parse(v).map_or_else(|| self.fail(key, what), Ok),
        }
    }

    fn id(&self, key: &str) -> Res<String> {
        self.req(key, "invalid id", |v| str_where(v, is_id))
    }

    fn name(&self, key: &str) -> Res<String> {
        self.req(key, "must be 1–64 characters", |v| {
            str_where(v, |s| (1..=64).contains(&len16(s)))
        })
    }

    fn bool_or(&self, key: &str, d: bool) -> Res<bool> {
        self.or(key, d, "expected a boolean", Value::as_bool)
    }

    fn icon(&self) -> String {
        catch(self.get("icon"), String::new(), |v| {
            str_where(v, |s| len16(s) <= 64)
        })
    }

    fn tint(&self) -> String {
        catch(self.get("color"), String::new(), |v| {
            str_where(v, |s| s.is_empty() || is_color(s))
        })
    }

    fn nullable_id_or_null(&self, key: &str) -> Res<Option<String>> {
        self.or(key, None, "invalid id", |v| {
            nullable(v, |v| str_where(v, is_id))
        })
    }

    fn catch_model(&self, key: &str) -> Option<String> {
        catch(self.get(key), None, |v| {
            nullable(v, |v| str_where(v, is_chat_model))
        })
    }

    fn catch_effort(&self, key: &str) -> Option<ChatEffort> {
        catch(self.get(key), None, |v| {
            nullable(v, |v| enum_of(v, ChatEffort::parse))
        })
    }

    fn catch_mode(&self, key: &str) -> Option<ChatMode> {
        catch(self.get(key), None, |v| {
            nullable(v, |v| enum_of(v, ChatMode::parse))
        })
    }

    fn extra(&self, known: &[&str]) -> Extra {
        self.map
            .iter()
            .filter(|(k, _)| !known.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

fn is_path(s: &str) -> bool {
    (1..=4096).contains(&len16(s))
}

fn array<'a>(v: &'a Value, at: &str, max: usize) -> Res<&'a Vec<Value>> {
    match v.as_array() {
        Some(a) if a.len() <= max => Ok(a),
        Some(_) => Err(format!("{at}: more than {max} items")),
        None => Err(format!("{at}: expected an array")),
    }
}

fn string_list(v: &Value, max_items: usize, ok: impl Fn(&str) -> bool) -> Option<Vec<String>> {
    let a = v.as_array().filter(|a| a.len() <= max_items)?;
    a.iter().map(|x| str_where(x, &ok)).collect()
}

// ---- The schema ---------------------------------------------------------------------------

fn parse_layout(v: &Value, at: &str) -> Res<LayoutNode> {
    let o = Obj::new(v, at)?;
    match o.get("type").and_then(Value::as_str) {
        Some("pane") => Ok(LayoutNode::Pane {
            pane_id: o.id("paneId")?,
        }),
        Some("empty") => Ok(LayoutNode::Empty {
            slot_id: o.id("slotId")?,
        }),
        Some("split") => {
            let dir = o.req("dir", "row or col", |v| enum_of(v, SplitDir::parse))?;
            let sizes = o.req("sizes", "sizes 0–100", |v| {
                v.as_array()?
                    .iter()
                    .map(|n| n.as_f64().filter(|f| (0.0..=100.0).contains(f)))
                    .collect::<Option<Vec<f64>>>()
            })?;
            let kids = o.get("children").and_then(Value::as_array);
            let Some(kids) = kids.filter(|k| (1..=16).contains(&k.len())) else {
                return o.fail("children", "1–16 children");
            };
            let children = kids
                .iter()
                .enumerate()
                .map(|(i, c)| parse_layout(c, &format!("{at}.children[{i}]")))
                .collect::<Res<Vec<_>>>()?;
            if sizes.len() != children.len() {
                return o.fail("sizes", "sizes must match children");
            }
            Ok(LayoutNode::Split {
                dir,
                sizes,
                children,
            })
        }
        _ => o.fail("type", "pane, empty or split"),
    }
}

const ACCOUNT_KEYS: &[&str] = &[
    "id",
    "name",
    "cli",
    "configDir",
    "color",
    "signedIn",
    "imported",
    "icon",
    "folderId",
];

fn parse_account(v: &Value, at: &str) -> Res<Account> {
    let o = Obj::new(v, at)?;
    Ok(Account {
        id: o.id("id")?,
        name: o.name("name")?,
        cli: catch(o.get("cli"), AgentCli::Claude, |v| {
            enum_of(v, AgentCli::parse)
        }),
        config_dir: o.req("configDir", "a path", |v| str_where(v, is_path))?,
        color: o.req("color", "#rrggbb", |v| str_where(v, is_color))?,
        signed_in: o.bool_or("signedIn", false)?,
        imported: o.bool_or("imported", false)?,
        icon: o.icon(),
        folder_id: o.nullable_id_or_null("folderId")?,
        extra: o.extra(ACCOUNT_KEYS),
    })
}

const FOLDER_KEYS: &[&str] = &["id", "name", "collapsed", "icon", "color"];

fn parse_folder(v: &Value, at: &str) -> Res<AccountFolder> {
    let o = Obj::new(v, at)?;
    Ok(AccountFolder {
        id: o.id("id")?,
        name: o.name("name")?,
        collapsed: o.bool_or("collapsed", false)?,
        icon: o.icon(),
        color: o.tint(),
        extra: o.extra(FOLDER_KEYS),
    })
}

const PANE_KEYS: &[&str] = &[
    "id",
    "accountId",
    "cwd",
    "args",
    "shell",
    "title",
    "resume",
    "hasConversation",
    "view",
    "chatSessionId",
    "model",
    "effort",
    "chatMode",
    "teamAgentId",
];

fn parse_pane(v: &Value, at: &str) -> Res<Pane> {
    let o = Obj::new(v, at)?;
    Ok(Pane {
        id: o.id("id")?,
        account_id: o.req("accountId", "invalid id", |v| {
            nullable(v, |v| str_where(v, is_id))
        })?,
        cwd: o.req("cwd", "a path", |v| str_where(v, is_path))?,
        args: o.or("args", vec![], "up to 32 arguments", |v| {
            string_list(v, 32, |s| len16(s) <= 1000)
        })?,
        shell: o.bool_or("shell", false)?,
        title: o.or("title", String::new(), "up to 128 characters", |v| {
            str_where(v, |s| len16(s) <= 128)
        })?,
        resume: o.bool_or("resume", true)?,
        has_conversation: o.bool_or("hasConversation", false)?,
        // Panes saved before chat views existed stay terminals (their agent may still run).
        view: catch(o.get("view"), PaneView::Terminal, |v| {
            enum_of(v, PaneView::parse)
        }),
        chat_session_id: catch(o.get("chatSessionId"), None, |v| {
            nullable(v, |v| str_where(v, is_chat_session_id))
        }),
        model: o.catch_model("model"),
        effort: o.catch_effort("effort"),
        chat_mode: o.catch_mode("chatMode"),
        team_agent_id: catch(o.get("teamAgentId"), None, |v| {
            nullable(v, |v| str_where(v, is_id))
        }),
        extra: o.extra(PANE_KEYS),
    })
}

fn parse_team_agent(v: &Value, at: &str) -> Res<TeamAgent> {
    let o = Obj::new(v, at)?;
    Ok(TeamAgent {
        id: o.id("id")?,
        role: o.req("role", "1–40 characters", |v| {
            v.as_str()
                .map(str::trim)
                .filter(|s| (1..=40).contains(&len16(s)))
                .map(String::from)
        })?,
        account_id: o.req("accountId", "invalid id", |v| {
            nullable(v, |v| str_where(v, is_id))
        })?,
        model: o.catch_model("model"),
        effort: o.catch_effort("effort"),
        chat_mode: o.catch_mode("chatMode"),
        prompt: o.or("prompt", String::new(), "up to 4000 characters", |v| {
            str_where(v, |s| len16(s) <= 4000)
        })?,
        folder: catch(o.get("folder"), "worktree".into(), |v| {
            str_where(v, |s| s == "worktree" || s == "shared")
        }),
    })
}

const WORKSPACE_KEYS: &[&str] = &[
    "id",
    "name",
    "icon",
    "color",
    "panes",
    "layout",
    "machineId",
    "folder",
    "preview",
    "accountId",
    "history",
    "team",
    "worktreePanes",
];

fn parse_workspace(v: &Value, at: &str) -> Res<Workspace> {
    let o = Obj::new(v, at)?;
    let panes = o
        .get("panes")
        .ok_or_else(|| format!("{at}.panes: required"))?;
    let panes = array(panes, &format!("{at}.panes"), 64)?
        .iter()
        .enumerate()
        .map(|(i, p)| parse_pane(p, &format!("{at}.panes[{i}]")))
        .collect::<Res<Vec<_>>>()?;
    let layout = match o.get("layout") {
        Some(Value::Null) => None,
        Some(l) => Some(parse_layout(l, &format!("{at}.layout"))?),
        None => return o.fail("layout", "required"),
    };
    let preview = catch(
        o.get("preview"),
        Preview {
            command: String::new(),
            url: String::new(),
        },
        |v| {
            let p = v.as_object()?;
            let field = |k: &str, max: usize| match p.get(k) {
                None => Some(String::new()),
                Some(x) => str_where(x, |s| len16(s) <= max),
            };
            Some(Preview {
                command: field("command", 1000)?,
                url: field("url", 2048)?,
            })
        },
    );
    let history = catch(o.get("history"), vec![], |v| {
        let a = v.as_array().filter(|a| a.len() <= 200)?;
        a.iter()
            .map(|h| {
                let h = Obj::new(h, "history").ok()?;
                let pane = parse_pane(h.get("pane")?, "history.pane").ok()?;
                let closed_at = h.get("closedAt").and_then(as_int).filter(|n| *n >= 0)?;
                Some(ArchivedChat { pane, closed_at })
            })
            .collect()
    });
    let team = catch(o.get("team"), vec![], |v| {
        let a = v.as_array().filter(|a| a.len() <= TEAM_MAX)?;
        a.iter().map(|t| parse_team_agent(t, "team").ok()).collect()
    });
    Ok(Workspace {
        id: o.id("id")?,
        name: o.name("name")?,
        icon: o.icon(),
        color: o.tint(),
        panes,
        layout,
        machine_id: o.nullable_id_or_null("machineId")?,
        folder: o.or("folder", String::new(), "up to 4096 characters", |v| {
            str_where(v, |s| len16(s) <= 4096)
        })?,
        preview,
        account_id: o.nullable_id_or_null("accountId")?,
        history,
        team,
        worktree_panes: catch(o.get("worktreePanes"), false, Value::as_bool),
        extra: o.extra(WORKSPACE_KEYS),
    })
}

fn parse_machine(v: &Value, at: &str) -> Res<Machine> {
    let o = Obj::new(v, at)?;
    Ok(Machine {
        id: o.id("id")?,
        name: o.name("name")?,
        ssh_host: o.req("sshHost", "not a valid ssh host", |v| {
            str_where(v, is_ssh_host)
        })?,
    })
}

/// Settings saved before #161 had `autoSwitchAtLimit: boolean`: on asks first now, off stays
/// off. The old key is then dropped with the other unknown ones.
pub fn migrate_settings(raw: &Value) -> Value {
    let Some(map) = raw.as_object() else {
        return raw.clone();
    };
    if map.contains_key("autoSwitchMode") {
        return raw.clone();
    }
    let Some(on) = map.get("autoSwitchAtLimit").and_then(Value::as_bool) else {
        return raw.clone();
    };
    let mut out = map.clone();
    out.insert(
        "autoSwitchMode".into(),
        Value::from(if on { "ask" } else { "off" }),
    );
    Value::Object(out)
}

const SETTINGS_KEYS: &[&str] = &[
    "fontFamily",
    "fontSize",
    "uiZoom",
    "theme",
    "themeDark",
    "themeLight",
    "accent",
    "terminalPalette",
    "defaultAccountId",
    "defaultCwd",
    "pinnedFolders",
    "sidebarCollapsed",
    "sidebarOpenProjects",
    "sidebarWidth",
    "diffWidth",
    "sharedMode",
    "checkUpdatesOnLaunch",
    "notifyPanes",
    "autoSwitchMode",
    "autoSwitchAt",
    "shellPath",
    "shellArgs",
    "modDirs",
    "gpuTerminal",
    "onWindowClose",
    "cliAccess",
    "newPaneView",
    "chatOnlyMigrated",
    "chatModel",
    "chatEffort",
    "chatMode",
    "cliPaths",
    "shortcuts",
    // Dropped by zod as unknown after migrate_settings has read it.
    "autoSwitchAtLimit",
];

fn int_in(v: &Value, min: i64, max: i64) -> Option<i64> {
    as_int(v).filter(|n| (min..=max).contains(n))
}

fn no_nul(s: &str) -> bool {
    !s.contains('\0')
}

pub fn parse_settings(raw: &Value) -> Res<Settings> {
    let v = migrate_settings(raw);
    let o = Obj::new(&v, "settings")?;
    let d = Settings::default();
    Ok(Settings {
        font_family: o.or("fontFamily", d.font_family, "unknown font", |v| {
            enum_of(v, TerminalFont::parse)
        })?,
        font_size: o.or("fontSize", d.font_size, "10–20", |v| {
            int_in(v, FONT_SIZE_MIN, FONT_SIZE_MAX)
        })?,
        ui_zoom: catch(o.get("uiZoom"), 1.0, |v| {
            v.as_f64().filter(|z| (0.67..=2.0).contains(z))
        }),
        theme: o.or("theme", d.theme, "system, dark or light", |v| {
            enum_of(v, ThemePreference::parse)
        })?,
        theme_dark: catch(o.get("themeDark"), d.theme_dark, |v| str_where(v, is_id)),
        theme_light: catch(o.get("themeLight"), d.theme_light, |v| str_where(v, is_id)),
        accent: o.or("accent", d.accent, "unknown accent", |v| {
            enum_of(v, Accent::parse)
        })?,
        terminal_palette: o.or(
            "terminalPalette",
            d.terminal_palette,
            "unknown palette",
            |v| enum_of(v, TerminalPalette::parse),
        )?,
        default_account_id: o.nullable_id_or_null("defaultAccountId")?,
        default_cwd: o.or("defaultCwd", d.default_cwd, "a path", |v| {
            str_where(v, is_path)
        })?,
        pinned_folders: catch(o.get("pinnedFolders"), vec![], |v| {
            string_list(v, 20, is_path)
        }),
        sidebar_collapsed: o.bool_or("sidebarCollapsed", false)?,
        sidebar_open_projects: catch(o.get("sidebarOpenProjects"), vec![], |v| {
            string_list(v, 500, is_id)
        }),
        sidebar_width: catch(o.get("sidebarWidth"), SIDEBAR_WIDTH_DEFAULT, |v| {
            int_in(v, SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX)
        }),
        diff_width: catch(o.get("diffWidth"), DIFF_WIDTH_DEFAULT, |v| {
            int_in(v, DIFF_WIDTH_MIN, DIFF_WIDTH_MAX)
        }),
        shared_mode: o.or("sharedMode", d.shared_mode, "overall or per-account", |v| {
            enum_of(v, SharedMode::parse)
        })?,
        check_updates_on_launch: o.bool_or("checkUpdatesOnLaunch", true)?,
        notify_panes: o.bool_or("notifyPanes", true)?,
        auto_switch_mode: catch(o.get("autoSwitchMode"), AutoSwitchMode::Ask, |v| {
            enum_of(v, AutoSwitchMode::parse)
        }),
        auto_switch_at: catch(o.get("autoSwitchAt"), 95, |v| int_in(v, 50, 100)),
        shell_path: o.or("shellPath", String::new(), "up to 4096 characters", |v| {
            str_where(v, |s| len16(s) <= 4096)
        })?,
        shell_args: o.or("shellArgs", vec![], "up to 32 arguments", |v| {
            string_list(v, 32, |s| len16(s) <= 1000 && no_nul(s))
        })?,
        mod_dirs: o.or("modDirs", vec![], "up to 16 folders", |v| {
            string_list(v, MOD_DIRS_MAX, |s| is_path(s) && no_nul(s))
        })?,
        gpu_terminal: o.bool_or("gpuTerminal", false)?,
        on_window_close: catch(o.get("onWindowClose"), CloseMode::Ask, |v| {
            enum_of(v, CloseMode::parse)
        }),
        cli_access: o.bool_or("cliAccess", false)?,
        new_pane_view: catch(o.get("newPaneView"), PaneView::Chat, |v| {
            enum_of(v, PaneView::parse)
        }),
        chat_only_migrated: o.bool_or("chatOnlyMigrated", false)?,
        chat_model: o.catch_model("chatModel"),
        chat_effort: o.catch_effort("chatEffort"),
        // Full access is only offered per chat, never by default: a saved one is dropped.
        chat_mode: o
            .catch_mode("chatMode")
            .filter(|m| *m != ChatMode::BypassPermissions),
        cli_paths: catch(o.get("cliPaths"), vec![], |v| {
            v.as_object()?
                .iter()
                .map(|(k, x)| {
                    Some((
                        AgentCli::parse(k)?,
                        str_where(x, |s| len16(s) <= 4096 && no_nul(s))?,
                    ))
                })
                .collect()
        }),
        shortcuts: catch(o.get("shortcuts"), vec![], |v| {
            v.as_object()?
                .iter()
                .map(|(k, x)| Some((ShortcutId::parse(k)?, str_where(x, is_shortcut)?)))
                .collect()
        }),
        extra: o
            .extra(SETTINGS_KEYS)
            .into_iter()
            .filter(|(k, _)| k != "autoSwitchAtLimit")
            .collect(),
    })
}

/// /^(?:mod\+)(?:shift\+)?(?:alt\+)?[A-Za-z0-9]{1,24}$/
fn is_shortcut(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("mod+") else {
        return false;
    };
    let rest = rest.strip_prefix("shift+").unwrap_or(rest);
    let rest = rest.strip_prefix("alt+").unwrap_or(rest);
    (1..=24).contains(&rest.len()) && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

/// One scheduled job; a bad job drops alone, not the whole list or the config.
fn parse_job(v: &Value) -> Option<ScheduledJob> {
    let o = Obj::new(v, "job").ok()?;
    let when = {
        let w = Obj::new(o.get("when")?, "when").ok()?;
        match w.get("kind")?.as_str()? {
            "once" => ScheduleWhen::Once {
                at: str_where(w.get("at")?, is_local_datetime)?,
            },
            "weekly" => {
                let days = w
                    .get("days")?
                    .as_array()
                    .filter(|d| (1..=7).contains(&d.len()))?;
                ScheduleWhen::Weekly {
                    days: days
                        .iter()
                        .map(|d| int_in(d, 0, 6))
                        .collect::<Option<_>>()?,
                    time: str_where(w.get("time")?, is_hhmm)?,
                }
            }
            "hours" => ScheduleWhen::Hours {
                every: int_in(w.get("every")?, 1, 24)?,
                from: str_where(w.get("from")?, is_hhmm)?,
            },
            "reset" => ScheduleWhen::Reset {
                window: str_where(w.get("window")?, |s| s == "fiveHour" || s == "sevenDay")?,
            },
            _ => return None,
        }
    };
    let target = catch(o.get("target"), ScheduleTarget::New, |v| {
        let t = v.as_object()?;
        match t.get("kind")?.as_str()? {
            "new" => Some(ScheduleTarget::New),
            "chat" => Some(ScheduleTarget::Chat {
                pane_id: str_where(t.get("paneId")?, is_id)?,
            }),
            _ => None,
        }
    });
    let runs = catch(o.get("runs"), vec![], |v| {
        let a = v.as_array().filter(|a| a.len() <= SCHEDULE_RUNS_MAX)?;
        a.iter()
            .map(|r| {
                let r = r.as_object()?;
                let result = ["running", "done", "failed", "missed", "skipped", "waiting"];
                Some(ScheduleRun {
                    at: r.get("at").and_then(as_int).filter(|n| *n >= 0)?,
                    result: str_where(r.get("result")?, |s| result.contains(&s))?,
                    pane_id: nullable(r.get("paneId")?, |v| str_where(v, is_id))?,
                    note: match r.get("note") {
                        None => String::new(),
                        Some(n) => str_where(n, |s| len16(s) <= 300)?,
                    },
                })
            })
            .collect()
    });
    let opt_bool = |k: &str, d: bool| match o.get(k) {
        None => Some(d),
        Some(v) => v.as_bool(),
    };
    Some(ScheduledJob {
        id: str_where(o.get("id")?, is_id)?,
        ws_id: str_where(o.get("wsId")?, is_id)?,
        account_id: str_where(o.get("accountId")?, is_id)?,
        prompt: o
            .get("prompt")?
            .as_str()
            .map(str::trim)
            .filter(|s| (1..=8000).contains(&len16(s)))?
            .to_string(),
        model: o.catch_model("model"),
        effort: o.catch_effort("effort"),
        chat_mode: o.catch_mode("chatMode"),
        when,
        target,
        at_limit: catch(o.get("atLimit"), "wait".into(), |v| {
            str_where(v, |s| s == "wait" || s == "switch")
        }),
        enabled: opt_bool("enabled", true)?,
        one_shot: opt_bool("oneShot", false)?,
        next_at: catch(o.get("nextAt"), None, |v| {
            nullable(v, |v| as_int(v).filter(|n| *n >= 0))
        }),
        runs,
    })
}

const CONFIG_KEYS: &[&str] = &[
    "version",
    "claudePath",
    "accounts",
    "accountFolders",
    "workspaces",
    "machines",
    "activeWorkspace",
    "recentFolders",
    "settings",
    "schedules",
];

fn parse_list<T>(
    o: &Obj,
    key: &str,
    max: usize,
    min: usize,
    required: bool,
    parse: impl Fn(&Value, &str) -> Res<T>,
) -> Res<Vec<T>> {
    let Some(v) = o.get(key) else {
        return if required {
            o.fail(key, "required")
        } else {
            Ok(vec![])
        };
    };
    let a = array(v, &format!("config.{key}"), max)?;
    if a.len() < min {
        return o.fail(key, &format!("at least {min} item"));
    }
    a.iter()
        .enumerate()
        .map(|(i, x)| parse(x, &format!("config.{key}[{i}]")))
        .collect()
}

fn parse_current(v: &Value) -> Res<Config> {
    let o = Obj::new(v, "config")?;
    if o.get("version").and_then(as_int) != Some(CONFIG_VERSION) {
        return o.fail("version", "must be 1");
    }
    Ok(Config {
        version: CONFIG_VERSION,
        claude_path: o.or("claudePath", String::new(), "up to 4096 characters", |v| {
            str_where(v, |s| len16(s) <= 4096)
        })?,
        accounts: parse_list(&o, "accounts", 64, 0, true, parse_account)?,
        account_folders: parse_list(&o, "accountFolders", 32, 0, false, parse_folder)?,
        workspaces: parse_list(&o, "workspaces", 32, 1, true, parse_workspace)?,
        machines: parse_list(&o, "machines", 32, 0, false, parse_machine)?,
        active_workspace: o.id("activeWorkspace")?,
        recent_folders: o.or("recentFolders", vec![], "up to 20 paths", |v| {
            string_list(v, 20, is_path)
        })?,
        settings: match o.get("settings") {
            None => Settings::default(),
            Some(s) => parse_settings(s)?,
        },
        schedules: catch(o.get("schedules"), vec![], |v| {
            let a = v.as_array().filter(|a| a.len() <= SCHEDULES_MAX)?;
            Some(a.iter().filter_map(parse_job).collect())
        }),
        extra: o.extra(CONFIG_KEYS),
    })
}

pub fn default_workspace() -> Workspace {
    Workspace {
        id: "ws-default".into(),
        name: "default".into(),
        icon: String::new(),
        color: String::new(),
        panes: vec![],
        layout: None,
        machine_id: None,
        folder: String::new(),
        preview: Preview {
            command: String::new(),
            url: String::new(),
        },
        account_id: None,
        history: vec![],
        team: vec![],
        worktree_panes: false,
        extra: Extra::new(),
    }
}

pub fn default_config() -> Config {
    Config {
        version: CONFIG_VERSION,
        claude_path: String::new(),
        accounts: vec![],
        account_folders: vec![],
        workspaces: vec![default_workspace()],
        machines: vec![],
        active_workspace: "ws-default".into(),
        recent_folders: vec![],
        settings: Settings::default(),
        schedules: vec![],
        extra: Extra::new(),
    }
}

/// Upgrades an older on-disk shape to the current version. Unversioned files are the
/// requirements §9 shape (v0). Returns the input untouched when it is already current.
pub fn migrate_config(raw: &Value) -> Value {
    let Some(map) = raw.as_object() else {
        return raw.clone();
    };
    if map.contains_key("version") {
        // Current, or an unknown future/past version: let validation decide.
        return raw.clone();
    }
    let arr = |k: &str| map.get(k).and_then(Value::as_array).cloned();
    let (accounts, workspaces) = (arr("accounts"), arr("workspaces"));
    if accounts.is_none() && workspaces.is_none() {
        return raw.clone();
    }
    let objects = |list: Option<Vec<Value>>| -> Vec<Map<String, Value>> {
        list.unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_object().cloned())
            .collect()
    };
    let migrated: Vec<Value> = objects(workspaces)
        .into_iter()
        .enumerate()
        .map(|(i, w)| {
            let id = w
                .get("id")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or(format!("ws-{}", i + 1));
            let name = w
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or(format!("workspace {}", i + 1));
            let panes: Vec<Value> = objects(w.get("panes").and_then(Value::as_array).cloned())
                .into_iter()
                .map(|mut p| {
                    if p.get("accountId").is_none_or(Value::is_null) {
                        p.insert("accountId".into(), Value::Null);
                    }
                    if !p.get("title").is_some_and(Value::is_string) {
                        let cwd = match p.get("cwd") {
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Null) | None => String::new(),
                            Some(other) => other.to_string(),
                        };
                        p.insert("title".into(), Value::from(crate::paths::base_name(&cwd)));
                    }
                    Value::Object(p)
                })
                .collect();
            // A v0 layout was an opaque object; a null layout is auto-arranged.
            serde_json::json!({ "id": id, "name": name, "panes": panes, "layout": null })
        })
        .collect();
    let accounts: Vec<Value> = objects(accounts)
        .into_iter()
        .enumerate()
        .map(|(i, mut a)| {
            if !a.get("color").is_some_and(Value::is_string) {
                a.insert(
                    "color".into(),
                    Value::from(ACCOUNT_COLORS[i % ACCOUNT_COLORS.len()].1),
                );
            }
            // Accounts configured before v1 were set up by hand and already used.
            if !a.get("signedIn").is_some_and(Value::is_boolean) {
                a.insert("signedIn".into(), Value::Bool(true));
            }
            if !a.get("imported").is_some_and(Value::is_boolean) {
                a.insert("imported".into(), Value::Bool(false));
            }
            Value::Object(a)
        })
        .collect();
    let claude_path = match map.get("claudePath").and_then(Value::as_str) {
        Some(p) if p != "claude" => p.to_string(),
        _ => String::new(),
    };
    let active = migrated
        .first()
        .and_then(|w| w["id"].as_str())
        .unwrap_or("ws-default")
        .to_string();
    let workspaces = if migrated.is_empty() {
        vec![serde_json::to_value(default_workspace()).unwrap()]
    } else {
        migrated
    };
    let settings = map
        .get("settings")
        .filter(|s| s.is_object())
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    serde_json::json!({
        "version": CONFIG_VERSION,
        "claudePath": claude_path,
        "accounts": accounts,
        "accountFolders": [],
        "workspaces": workspaces,
        "activeWorkspace": active,
        "recentFolders": [],
        "settings": settings,
    })
}

/// Repairs cross-references validation cannot express. Never fails.
pub fn sanitize_config(mut cfg: Config) -> Config {
    let folder_ids: Vec<String> = cfg.account_folders.iter().map(|f| f.id.clone()).collect();
    for a in &mut cfg.accounts {
        if a.folder_id
            .as_ref()
            .is_some_and(|f| !folder_ids.contains(f))
        {
            a.folder_id = None;
        }
    }
    let account_ids: Vec<String> = cfg.accounts.iter().map(|a| a.id.clone()).collect();
    let machine_ids: Vec<String> = cfg.machines.iter().map(|m| m.id.clone()).collect();
    let cli_of = |id: &Option<String>| {
        cfg.accounts
            .iter()
            .find(|a| Some(&a.id) == id.as_ref())
            .map_or(AgentCli::Claude, |a| a.cli)
    };
    // Chat-capable agent panes show as chats (#110): a saved terminal turns into one, once;
    // terminals made later stay terminals.
    let migrated = cfg.settings.chat_only_migrated;
    let to_chat = |p: &mut Pane, remote: bool, cli: AgentCli| {
        if !migrated && !remote && !p.shell && p.view == PaneView::Terminal && cli.spec().chat {
            p.view = PaneView::Chat;
        }
    };
    for w in &mut cfg.workspaces {
        if w.machine_id
            .as_ref()
            .is_some_and(|m| !machine_ids.contains(m))
        {
            w.machine_id = None;
        }
        let remote = w.machine_id.is_some();
        if w.account_id
            .as_ref()
            .is_some_and(|a| !account_ids.contains(a))
        {
            w.account_id = None;
        }
        for p in &mut w.panes {
            if p.account_id
                .as_ref()
                .is_some_and(|a| !account_ids.contains(a))
            {
                p.account_id = None;
            }
            let cli = cli_of(&p.account_id);
            to_chat(p, remote, cli);
        }
        for h in &mut w.history {
            let cli = cli_of(&h.pane.account_id);
            to_chat(&mut h.pane, remote, cli);
        }
    }
    if !cfg.workspaces.iter().any(|w| w.id == cfg.active_workspace) {
        cfg.active_workspace = cfg.workspaces[0].id.clone();
    }
    if cfg
        .settings
        .default_account_id
        .as_ref()
        .is_some_and(|a| !account_ids.contains(a))
    {
        cfg.settings.default_account_id = None;
    }
    cfg.settings.chat_only_migrated = true;
    cfg
}

/// Validates and repairs untrusted config input. Only files read from disk are migrated.
pub fn parse_config(raw: &Value, migrate: bool) -> Result<Config, String> {
    let v = if migrate {
        migrate_config(raw)
    } else {
        raw.clone()
    };
    parse_current(&v).map(sanitize_config)
}

/// A pane folder under `~/.wraithgrid/worktrees/` (`isWorktreeCwd`): a branch of its own.
pub fn is_worktree_cwd(cwd: &str) -> bool {
    cwd.replace('\\', "/")
        .starts_with("~/.wraithgrid/worktrees/")
}

impl Workspace {
    /// Where the project's new chats start (`projectFolder`): its folder, else the folder of
    /// its newest open chat, else of its most recently closed one (worktrees never count).
    pub fn project_folder(&self) -> Option<String> {
        if !self.folder.is_empty() {
            return Some(self.folder.clone());
        }
        let usable = |p: &Pane| !p.shell && !p.cwd.is_empty() && !is_worktree_cwd(&p.cwd);
        if let Some(p) = self.panes.iter().rev().find(|p| usable(p)) {
            return Some(p.cwd.clone());
        }
        let mut closed: Vec<&ArchivedChat> = self.history.iter().collect();
        closed.sort_by_key(|h| std::cmp::Reverse(h.closed_at));
        closed
            .into_iter()
            .find(|h| usable(&h.pane))
            .map(|h| h.pane.cwd.clone())
    }
}

impl Config {
    /// The file's text: `JSON.stringify(config, null, 2) + '\n'`.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("config serializes");
        s.push('\n');
        s
    }

    pub fn active(&self) -> &Workspace {
        self.workspaces
            .iter()
            .find(|w| w.id == self.active_workspace)
            .unwrap_or(&self.workspaces[0])
    }

    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal() -> Value {
        json!({
            "version": 1,
            "accounts": [{ "id": "a1", "name": "Work", "configDir": "~/.wraithgrid/accounts/a1", "color": "#7c5cff" }],
            "workspaces": [{ "id": "ws1", "name": "proj", "panes": [{ "id": "p1", "accountId": "a1", "cwd": "~" }], "layout": { "type": "pane", "paneId": "p1" } }],
            "activeWorkspace": "ws1"
        })
    }

    #[test]
    fn defaults_fill_in() {
        let c = parse_config(&minimal(), true).unwrap();
        assert_eq!(c.settings.font_size, 13);
        assert_eq!(c.accounts[0].cli, AgentCli::Claude);
        let p = &c.workspaces[0].panes[0];
        assert!(p.resume && !p.shell);
        // A saved terminal of a chat-capable CLI turns into a chat once (#110).
        assert_eq!(p.view, PaneView::Chat);
        assert!(c.settings.chat_only_migrated);
    }

    #[test]
    fn catch_fields_fall_back() {
        let mut v = minimal();
        v["settings"] = json!({ "uiZoom": 9, "sidebarWidth": 5, "autoSwitchMode": "nope", "cliPaths": { "bogus": "x" } });
        v["workspaces"][0]["panes"][0]["model"] = json!("bad model!");
        v["workspaces"][0]["color"] = json!("red");
        let c = parse_config(&v, true).unwrap();
        assert_eq!(c.settings.ui_zoom, 1.0);
        assert_eq!(c.settings.sidebar_width, SIDEBAR_WIDTH_DEFAULT);
        assert_eq!(c.settings.auto_switch_mode, AutoSwitchMode::Ask);
        assert!(c.settings.cli_paths.is_empty());
        assert_eq!(c.workspaces[0].panes[0].model, None);
        assert_eq!(c.workspaces[0].color, "");
    }

    #[test]
    fn full_access_is_never_the_default_mode() {
        let mut v = minimal();
        v["settings"] = json!({ "chatMode": "bypassPermissions" });
        assert_eq!(parse_config(&v, true).unwrap().settings.chat_mode, None);
        v["settings"] = json!({ "chatMode": "plan" });
        let c = parse_config(&v, true).unwrap();
        assert_eq!(c.settings.chat_mode, Some(ChatMode::Plan));
    }

    #[test]
    fn strict_fields_reject_the_file() {
        let mut v = minimal();
        v["settings"] = json!({ "fontSize": 99 });
        assert!(parse_config(&v, true).is_err());
        let mut v = minimal();
        v["workspaces"] = json!([]);
        assert!(parse_config(&v, true).is_err());
        let mut v = minimal();
        v["accounts"][0]["id"] = json!("has space");
        assert!(parse_config(&v, true).is_err());
        let mut v = minimal();
        v["version"] = json!(2);
        assert!(parse_config(&v, true).is_err());
    }

    #[test]
    fn sanitize_repairs_references() {
        let mut v = minimal();
        v["workspaces"][0]["panes"][0]["accountId"] = json!("gone");
        v["activeWorkspace"] = json!("missing");
        v["settings"] = json!({ "defaultAccountId": "gone", "chatOnlyMigrated": true });
        v["accounts"][0]["folderId"] = json!("nofolder");
        let c = parse_config(&v, true).unwrap();
        assert_eq!(c.workspaces[0].panes[0].account_id, None);
        assert_eq!(c.workspaces[0].panes[0].view, PaneView::Terminal);
        assert_eq!(c.active_workspace, "ws1");
        assert_eq!(c.settings.default_account_id, None);
        assert_eq!(c.accounts[0].folder_id, None);
    }

    #[test]
    fn auto_switch_migration() {
        let s = parse_settings(&json!({ "autoSwitchAtLimit": false })).unwrap();
        assert_eq!(s.auto_switch_mode, AutoSwitchMode::Off);
        assert!(s.extra.is_empty());
        let s = parse_settings(&json!({ "autoSwitchAtLimit": true })).unwrap();
        assert_eq!(s.auto_switch_mode, AutoSwitchMode::Ask);
    }

    #[test]
    fn v0_files_are_migrated() {
        let v0 = json!({
            "claudePath": "claude",
            "accounts": [{ "id": "a", "name": "A", "configDir": "~/a" }],
            "workspaces": [{ "id": "w", "panes": [{ "id": "p", "cwd": "/home/u/proj" }], "layout": { "x": 1 } }]
        });
        let c = parse_config(&v0, true).unwrap();
        assert_eq!(c.claude_path, "");
        assert!(c.accounts[0].signed_in);
        assert_eq!(c.accounts[0].color, "#7c5cff");
        assert_eq!(c.workspaces[0].name, "workspace 1");
        assert_eq!(c.workspaces[0].panes[0].title, "proj");
        assert_eq!(c.workspaces[0].layout, None);
    }

    #[test]
    fn unknown_keys_survive_and_order_is_schema_order() {
        let mut v = minimal();
        v["futureField"] = json!({ "a": 1 });
        v["settings"] = json!({ "newSetting": true, "uiZoom": 1.25 });
        let c = parse_config(&v, true).unwrap();
        let text = c.to_json();
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["futureField"], json!({ "a": 1 }));
        assert_eq!(back["settings"]["newSetting"], json!(true));
        assert!(
            text.starts_with("{\n  \"version\": 1,\n  \"claudePath\": \"\",\n  \"accounts\": [")
        );
        assert!(text.contains("\"uiZoom\": 1.25"));
        // Whole numbers are written as JavaScript writes them.
        assert!(parse_config(&minimal(), true)
            .unwrap()
            .to_json()
            .contains("\"uiZoom\": 1,"));
        // Round trip is stable.
        assert_eq!(parse_config(&back, false).unwrap(), c);
    }

    #[test]
    fn bad_schedule_drops_alone() {
        let mut v = minimal();
        v["schedules"] = json!([
            { "id": "s1", "wsId": "ws1", "accountId": "a1", "prompt": "  hi  ", "when": { "kind": "weekly", "days": [1, 3], "time": "09:30" } },
            { "id": "s2", "wsId": "ws1", "accountId": "a1", "prompt": "x", "when": { "kind": "weekly", "days": [], "time": "09:30" } }
        ]);
        let c = parse_config(&v, true).unwrap();
        assert_eq!(c.schedules.len(), 1);
        assert_eq!(c.schedules[0].prompt, "hi");
        assert_eq!(c.schedules[0].target, ScheduleTarget::New);
    }

    #[test]
    fn layout_rules() {
        let mut v = minimal();
        v["workspaces"][0]["layout"] = json!({ "type": "split", "dir": "row", "sizes": [50, 50], "children": [{ "type": "pane", "paneId": "p1" }, { "type": "empty", "slotId": "s" }] });
        let c = parse_config(&v, true).unwrap();
        assert!(c.to_json().contains("\"sizes\": [\n"));
        v["workspaces"][0]["layout"]["sizes"] = json!([100]);
        assert!(parse_config(&v, true).is_err());
    }
}
