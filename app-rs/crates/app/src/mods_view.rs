//! Mods (`ModsView.tsx`): the claude accounts' mods (claude plugins) as one list, switched,
//! updated, uninstalled and handed to every account at once; finding one in the marketplaces
//! or on claudemod.com; the marketplaces themselves; and mod folders every claude pane loads.
//! The work is the `wraithgrid-mods` crate's `ModManager`, run off the UI thread through each
//! account's own `claude plugin` commands.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::a11y::A11y as _;
use crate::settings::SettingsPatch;
use wraithgrid_core::config::Config;
use wraithgrid_core::paths::{contract_home, home_dir};
use wraithgrid_mods::catalog::{
    clone_mod, download_dir, format_installs, installed_everywhere, merge_mods, pull_mod,
    search_mods, MergedMod,
};
use wraithgrid_mods::mods::{downloaded_mod_name, marketplace_of, mod_name_of, ClaudeCli};
use wraithgrid_mods::{
    AccountMods, CatalogMod, CommunityCatalog, FailedAccount, MarketplaceList, ModChange, ModDeps,
    ModManager,
};

/// How many catalog rows show before "Show more".
const CATALOG_PAGE: usize = 20;
/// `MOD_DIRS_MAX`.
const MOD_DIRS_MAX: usize = 16;

pub enum ModsViewEvent {
    Close,
}

/// What the page asks the window for, beyond closing (a separate event, so the window's
/// `ModsViewEvent` handler stays as it is).
// The window reads it once it subscribes to this event (shell.rs follow-up).
#[allow(dead_code)]
pub enum ModsRequest {
    /// Mod folders changed (`settings.modDirs`): apply and save.
    Config(SettingsPatch),
    /// "Restart claude panes": the window restarts them and answers with `set_restarted`.
    RestartClaudePanes,
    /// No claude account yet: open Accounts with its add form.
    AddAccount,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Marketplaces,
    Claudemod,
}

pub struct ModsView {
    focus: FocusHandle,
    config: Config,
    manager: Arc<ModManager>,
    installed: Option<Vec<AccountMods>>,
    loading: bool,
    source: Source,
    market: Option<Result<Vec<CatalogMod>, String>>,
    community: Option<Result<Vec<CatalogMod>, String>>,
    /// Made at the first draw (an input needs the window).
    query: Option<Entity<InputState>>,
    limit: usize,
    marketplaces: Option<Result<MarketplaceList, String>>,
    market_source: Option<Entity<InputState>>,
    confirm_market: Option<String>,
    confirm_uninstall: Option<String>,
    /// The row (mod key, marketplace or catalog id) working now; one at a time.
    busy: Option<String>,
    /// What the last action on a row came to.
    errors: HashMap<String, String>,
    /// Something changed that running panes only load at start.
    changed: bool,
    restarted: Option<usize>,
    /// Per downloaded mod: None while its update runs, else what it said.
    pulled: HashMap<String, Option<String>>,
    _subscriptions: Vec<Subscription>,
}

/// Runs `f` with the mods crate's view of the app: accounts, claude, each account's env.
fn with_deps<R>(config: &Config, f: impl FnOnce(&ModDeps<'_>) -> R) -> R {
    let env = crate::runtime::spawn_env();
    let claude = crate::mods_glue::claude_binary(config, &env).map(ClaudeCli::new);
    let accounts = crate::mods_glue::accounts(config);
    let env_for = |a: &wraithgrid_mods::Account| crate::mods_glue::env_for(config, &env, a);
    let log = |m: &str| eprintln!("[wraithgrid] mods: {m}");
    let deps = ModDeps {
        accounts: &accounts,
        claude: claude.as_ref().map(|c| c as _),
        env_for: &env_for,
        log: &log,
    };
    f(&deps)
}

/// What an action for every account came to, as one line; None when all took it.
fn failed_line(res: Result<Vec<FailedAccount>, String>) -> Option<String> {
    match res {
        Err(e) => Some(e),
        Ok(failed) => failed.first().map(|f| {
            let more = failed.len() - 1;
            if more > 0 {
                format!("{}: {} (and {more} more)", f.account, f.error)
            } else {
                format!("{}: {}", f.account, f.error)
            }
        }),
    }
}

fn row_key(m: &MergedMod) -> String {
    format!(
        "{}|{}|{}",
        m.info.id,
        m.info.scope,
        m.info.project_path.as_deref().unwrap_or("")
    )
}

impl ModsView {
    pub fn new(config: Config, cx: &mut Context<Self>) -> Self {
        let manager = Arc::new(ModManager::new(
            home_dir(),
            CommunityCatalog::new(wraithgrid_mods::mod_community::ureq_fetch),
        ));
        let mut view = ModsView {
            focus: cx.focus_handle(),
            config,
            manager,
            installed: None,
            loading: false,
            source: Source::Marketplaces,
            market: None,
            community: None,
            query: None,
            limit: CATALOG_PAGE,
            marketplaces: None,
            market_source: None,
            confirm_market: None,
            confirm_uninstall: None,
            busy: None,
            errors: HashMap::new(),
            changed: false,
            restarted: None,
            pulled: HashMap::new(),
            _subscriptions: vec![],
        };
        view.refresh(cx);
        view.load_catalog(cx);
        view
    }

    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.is_some() {
            return;
        }
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search mods by name or what they do")
        });
        let market_source =
            cx.new(|cx| InputState::new(window, cx).placeholder("owner/repo or https://…"));
        self._subscriptions = vec![
            cx.subscribe(&query, |this, _, e: &InputEvent, cx| {
                if let InputEvent::Change = e {
                    this.limit = CATALOG_PAGE;
                    cx.notify();
                }
            }),
            cx.subscribe(&market_source, |this, _, e: &InputEvent, cx| match e {
                InputEvent::PressEnter { .. } => this.add_marketplace(cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
        ];
        self.query = Some(query);
        self.market_source = Some(market_source);
    }

    fn text(input: &Option<Entity<InputState>>, cx: &App) -> String {
        input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_string())
            .unwrap_or_default()
    }

    /// The config after the window applied a change.
    // Called by the window (shell.rs follow-up).
    #[allow(dead_code)]
    pub fn set_config(&mut self, config: Config, cx: &mut Context<Self>) {
        self.config = config;
        cx.notify();
    }

    /// How many panes "Restart claude panes" restarted.
    // Called by the window on `ModsRequest::RestartClaudePanes` (shell.rs follow-up).
    #[allow(dead_code)]
    pub fn set_restarted(&mut self, n: usize, cx: &mut Context<Self>) {
        self.restarted = Some(n);
        cx.notify();
    }

    fn claude_accounts(&self) -> Vec<(String, String)> {
        self.config
            .accounts
            .iter()
            .filter(|a| a.cli == wraithgrid_core::agent_cli::AgentCli::Claude)
            .map(|a| (a.id.clone(), a.name.clone()))
            .collect()
    }

    fn name_of(&self, id: &str) -> String {
        self.config
            .account(id)
            .map_or_else(|| id.to_string(), |a| a.name.clone())
    }

    /// The installed mods and the marketplaces, asked again.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        let (config, manager) = (self.config.clone(), self.manager.clone());
        cx.spawn(async move |this, cx| {
            let (installed, markets) = cx
                .background_spawn(async move {
                    with_deps(&config, |d| (manager.list(d), manager.marketplaces(d)))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.installed = Some(installed);
                this.marketplaces = Some(markets);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The marketplaces' catalog; claudemod.com only once it is asked for (another site).
    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let (config, manager, source) = (self.config.clone(), self.manager.clone(), self.source);
        cx.spawn(async move |this, cx| {
            let list = cx
                .background_spawn(async move {
                    match source {
                        Source::Marketplaces => with_deps(&config, |d| manager.catalog(d)),
                        Source::Claudemod => manager.community(),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match source {
                    Source::Marketplaces => this.market = Some(list),
                    Source::Claudemod => this.community = Some(list),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// One change for every account in the background, then the lists again; its outcome is
    /// shown on the row `key`.
    fn run(
        &mut self,
        key: String,
        job: impl FnOnce(&ModManager, &ModDeps<'_>) -> Option<String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(key.clone());
        self.errors.remove(&key);
        let (config, manager) = (self.config.clone(), self.manager.clone());
        cx.spawn(async move |this, cx| {
            let error = cx
                .background_spawn(async move { with_deps(&config, |d| job(&manager, d)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match error {
                    Some(e) => {
                        this.errors.insert(key, e);
                    }
                    None => {
                        this.changed = true;
                        this.restarted = None;
                    }
                }
                this.refresh(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// One switch for every account that has the mod; an account's refusal is shown by name.
    fn toggle(&mut self, row: MergedMod, cx: &mut Context<Self>) {
        let enabled = !row.info.enabled;
        let names: Vec<(String, String)> = row
            .accounts
            .iter()
            .map(|id| (id.clone(), self.name_of(id)))
            .collect();
        let many = self.claude_accounts().len() > 1;
        self.run(
            row_key(&row),
            move |m, d| {
                let mut failed = None;
                for (id, name) in &names {
                    if row.on.contains(id) == enabled {
                        continue;
                    }
                    let i = &row.info;
                    if let Err(e) =
                        m.set_enabled(d, id, &i.id, &i.scope, i.project_path.as_deref(), enabled)
                    {
                        failed.get_or_insert(if many { format!("{name}: {e}") } else { e });
                    }
                }
                failed
            },
            cx,
        );
    }

    fn install(&mut self, m: CatalogMod, cx: &mut Context<Self>) {
        // A bare plugin is downloaded and loaded as a mod folder, in every claude pane.
        if m.clone.is_some() {
            let key = m.id.clone();
            if self.busy.is_some() {
                return;
            }
            self.busy = Some(key.clone());
            self.errors.remove(&key);
            cx.spawn(async move |this, cx| {
                let got = cx
                    .background_spawn(async move {
                        let env = crate::runtime::spawn_env().into_iter().collect();
                        clone_mod(&home_dir(), &m, &env)
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.busy = None;
                    match got {
                        Ok(dir) => this.add_dir(dir, cx),
                        Err(e) => {
                            this.errors.insert(key, e);
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
            return cx.notify();
        }
        let id = m.id.clone();
        self.run(id.clone(), move |mm, d| failed_line(mm.install(d, &id)), cx);
    }

    fn add_marketplace(&mut self, cx: &mut Context<Self>) {
        let source = Self::text(&self.market_source, cx);
        if source.is_empty() {
            return;
        }
        self.run(
            "market-add".into(),
            move |m, d| failed_line(m.add_marketplace(d, &source)),
            cx,
        );
    }

    fn set_dirs(&mut self, dirs: Vec<String>, cx: &mut Context<Self>) {
        self.config.settings.mod_dirs = dirs.clone();
        self.changed = true;
        self.restarted = None;
        cx.emit(ModsRequest::Config(Arc::new(move |c| {
            c.settings.mod_dirs = dirs.clone()
        })));
        cx.notify();
    }

    fn add_dir(&mut self, dir: String, cx: &mut Context<Self>) {
        let mut dirs = self.config.settings.mod_dirs.clone();
        if !dirs.contains(&dir) && dirs.len() < MOD_DIRS_MAX {
            dirs.push(dir);
            self.set_dirs(dirs, cx);
        }
    }

    fn pick_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let home = home_dir().to_string_lossy().into_owned();
            let dir = contract_home(&dir.to_string_lossy(), &home);
            let _ = this.update(cx, |this, cx| this.add_dir(dir, cx));
        })
        .detach();
    }

    fn pull(&mut self, name: String, cx: &mut Context<Self>) {
        self.pulled.insert(name.clone(), None);
        cx.spawn(async move |this, cx| {
            let n = name.clone();
            let r = cx
                .background_spawn(async move {
                    let env = crate::runtime::spawn_env().into_iter().collect();
                    pull_mod(&home_dir(), &n, &env)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let said = match r {
                    Ok(true) => {
                        this.changed = true;
                        "Updated".to_string()
                    }
                    Ok(false) => "Up to date".to_string(),
                    Err(e) => e,
                };
                this.pulled.insert(name, Some(said));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl EventEmitter<ModsViewEvent> for ModsView {}
impl EventEmitter<ModsRequest> for ModsView {}

impl Focusable for ModsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().clone();
    let label = label.into();
    div()
        .id(id.into())
        .a11y_button(label.clone())
        .px_2()
        .py(px(2.))
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .text_xs()
        .flex_shrink_0()
        .when(enabled, |d| {
            d.cursor_pointer().hover(|d| d.bg(theme.secondary))
        })
        .when(!enabled, |d| d.opacity(0.45))
        .child(label)
}

fn section(title: &'static str, explain: String, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .max_w(px(820.))
        .flex()
        .flex_col()
        .gap_2()
        .pb_4()
        .border_b_1()
        .border_color(theme.border)
        .child(div().font_weight(FontWeight::MEDIUM).child(title))
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(explain),
        )
}

fn group(title: &'static str, cx: &App) -> Div {
    div()
        .pt_2()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(title.to_uppercase())
}

impl ModsView {
    fn installed_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let accounts = self.claude_accounts();
        let hint = |t: String| div().text_sm().text_color(theme.muted_foreground).child(t);
        let mut body: Vec<AnyElement> = Vec::new();
        let results = self.installed.clone();
        for r in results.iter().flatten() {
            if let Err(e) = &r.mods {
                body.push(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(format!("{}: {e}", self.name_of(&r.account_id)))
                        .into_any_element(),
                );
            }
        }
        match &results {
            None => body.push(hint("Asking claude…".into()).into_any_element()),
            Some(results) => {
                let rows = merge_mods(results);
                let any_ok = results.iter().any(|r| r.mods.is_ok());
                if rows.is_empty() && (any_ok || results.is_empty()) {
                    body.push(
                        hint("No mods installed. Find one under Browse below.".into())
                            .into_any_element(),
                    );
                }
                for row in rows {
                    body.push(self.mod_row(row, accounts.len(), cx));
                }
            }
        }
        section(
            if accounts.len() > 1 {
                "All claude accounts"
            } else {
                "Your claude account"
            },
            if accounts.len() > 1 {
                format!(
                    "One list for all {} accounts: a switch turns a mod on or off in each account that has it.",
                    accounts.len()
                )
            } else {
                format!(
                    "The mods {} has, as its own claude lists them.",
                    accounts.first().map_or("this account", |(_, n)| n.as_str())
                )
            },
            cx,
        )
        .children(body)
        .into_any_element()
    }

    fn mod_row(&self, row: MergedMod, total: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let key = row_key(&row);
        let busy = self.busy.as_deref() == Some(key.as_str());
        let idle = self.busy.is_none();
        let name = mod_name_of(&row.info.id).to_string();
        let home = home_dir().to_string_lossy().into_owned();
        let where_ = if total < 2 {
            None
        } else if row.missing.is_empty() {
            Some(format!("all {} accounts", row.accounts.len()))
        } else {
            Some(format!(
                "only {}",
                row.accounts
                    .iter()
                    .map(|a| self.name_of(a))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        };
        let partly = (!row.on.is_empty() && row.on.len() < row.accounts.len()).then(|| {
            format!(
                "on for {}",
                row.on
                    .iter()
                    .map(|a| self.name_of(a))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        let detail = [
            Some(marketplace_of(&row.info.id).to_string()),
            Some(row.info.scope.clone()),
            row.info
                .project_path
                .as_ref()
                .map(|p| contract_home(p, &home)),
            where_,
            partly,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        // Only a mod installed for the whole account can be handed to the other accounts.
        let spread = row.info.scope == "user" && !row.missing.is_empty();
        let confirming = self.confirm_uninstall.as_deref() == Some(key.as_str());
        let accounts_with = row.accounts.len();
        let url = row.info.url.clone();
        let (k_spread, k_update, k_remove, k_toggle) =
            (key.clone(), key.clone(), key.clone(), key.clone());
        let (id_spread, id_update, id_remove) = (
            row.info.id.clone(),
            row.info.id.clone(),
            row.info.id.clone(),
        );
        let toggle_row = row.clone();
        div()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id(SharedString::from(format!("mod-name-{key}")))
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .when_some(url, |d, url| {
                                        d.role(Role::Link)
                                            .aria_label(format!("Open the repository of {name}"))
                                            .cursor_pointer()
                                            .text_color(theme.link)
                                            .on_click(move |_, _, cx| cx.open_url(&url))
                                    })
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(row.info.version.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    )
                    .children(row.info.notes.iter().map(|n| {
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(n.clone())
                    }))
                    .when_some(self.errors.get(&key).cloned(), |d, e| {
                        d.child(div().text_xs().text_color(theme.danger).child(e))
                    }),
            )
            .when(spread, |d| {
                d.child(
                    button(
                        format!("spread-{key}"),
                        if busy {
                            "Adding…"
                        } else {
                            "Add to all accounts"
                        },
                        idle,
                        cx,
                    )
                    .when(idle, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            let id = id_spread.clone();
                            this.run(
                                k_spread.clone(),
                                move |m, d| failed_line(m.install(d, &id)),
                                cx,
                            )
                        }))
                    }),
                )
            })
            .child(
                button(format!("update-{key}"), "Update", idle, cx).when(idle, |d| {
                    d.tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Update to the newest version")
                            .build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id_update.clone();
                        this.run(
                            k_update.clone(),
                            move |m, d| failed_line(m.change(d, &id, ModChange::Update)),
                            cx,
                        )
                    }))
                }),
            )
            .child(
                button(
                    format!("uninstall-{key}"),
                    if confirming {
                        if accounts_with > 1 {
                            format!("Remove from {accounts_with} accounts")
                        } else {
                            "Click to remove".to_string()
                        }
                    } else {
                        "Uninstall".to_string()
                    },
                    idle,
                    cx,
                )
                .when(confirming, |d| {
                    d.text_color(theme.danger).border_color(theme.danger)
                })
                .when(idle, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        if this.confirm_uninstall.as_deref() != Some(k_remove.as_str()) {
                            this.confirm_uninstall = Some(k_remove.clone());
                            return cx.notify();
                        }
                        this.confirm_uninstall = None;
                        let id = id_remove.clone();
                        this.run(
                            k_remove.clone(),
                            move |m, d| failed_line(m.change(d, &id, ModChange::Uninstall)),
                            cx,
                        )
                    }))
                }),
            )
            .child(
                Switch::new(SharedString::from(format!("enabled-{k_toggle}")))
                    .checked(row.info.enabled)
                    .accessibility_label(format!("{name} enabled"))
                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                        this.toggle(toggle_row.clone(), cx)
                    })),
            )
            .into_any_element()
    }

    fn browse_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let accounts = self.claude_accounts().len();
        let source = self.source;
        let res = match source {
            Source::Marketplaces => &self.market,
            Source::Claudemod => &self.community,
        };
        let query = Self::text(&self.query, cx);
        let chip =
            |id: &'static str, label: &'static str, on: bool, s: Source, cx: &mut Context<Self>| {
                div()
                    .id(id)
                    .a11y_toggle(label, on)
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .text_sm()
                    .cursor_pointer()
                    .border_color(if on { theme.ring } else { theme.border })
                    .when(on, |d| d.bg(theme.secondary))
                    .hover(|d| d.bg(theme.secondary))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.source = s;
                        this.limit = CATALOG_PAGE;
                        let missing = match s {
                            Source::Marketplaces => this.market.is_none(),
                            Source::Claudemod => this.community.is_none(),
                        };
                        if missing {
                            this.load_catalog(cx);
                        }
                        cx.notify();
                    }))
            };
        let hint = |t: String| div().text_sm().text_color(theme.muted_foreground).child(t);
        let mut body: Vec<AnyElement> = Vec::new();
        match res {
            None => body.push(
                hint(match source {
                    Source::Claudemod => "Asking claudemod.com…".into(),
                    Source::Marketplaces => "Asking claude for the catalog…".into(),
                })
                .into_any_element(),
            ),
            Some(Err(e)) => body.push(
                div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(e.clone())
                    .into_any_element(),
            ),
            Some(Ok(list)) => {
                let found = search_mods(list, &query);
                if found.is_empty() {
                    body.push(
                        hint(if query.is_empty() {
                            "No mod to install here.".into()
                        } else {
                            format!("No mod matches “{query}”.")
                        })
                        .into_any_element(),
                    );
                } else {
                    body.push(
                        hint(if !query.is_empty() {
                            format!(
                                "{} {} found",
                                found.len(),
                                if found.len() == 1 { "mod" } else { "mods" }
                            )
                        } else if source == Source::Claudemod {
                            format!("{} mods from claudemod.com, a community list: their marketplace is added on install", found.len())
                        } else {
                            format!("{} mods, most installed first", found.len())
                        })
                        .into_any_element(),
                    );
                    let more = found.len() > self.limit;
                    for m in found.into_iter().take(self.limit) {
                        body.push(self.catalog_row(m, cx));
                    }
                    if more {
                        body.push(
                            button("catalog-more", "Show more", true, cx)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.limit += CATALOG_PAGE;
                                    cx.notify();
                                }))
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        let a = chip(
            "source-markets",
            "Marketplaces",
            source == Source::Marketplaces,
            Source::Marketplaces,
            cx,
        );
        let b = chip(
            "source-claudemod",
            "claudemod.com",
            source == Source::Claudemod,
            Source::Claudemod,
            cx,
        );
        section(
            "Find a mod",
            format!(
                "Everything the marketplaces offer, or the mods listed on claudemod.com. {}A mod runs as you, with your files and logins: install the ones you trust.",
                if accounts > 1 {
                    format!("Install puts a mod in all {accounts} accounts. ")
                } else {
                    String::new()
                }
            ),
            cx,
        )
        .child(div().flex().gap_2().child(a).child(b))
        .children(self.query.as_ref().map(Input::new))
        .children(body)
        .into_any_element()
    }

    fn catalog_row(&self, m: CatalogMod, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let busy = self.busy.as_deref() == Some(m.id.as_str());
        let idle = self.busy.is_none();
        let state = if m.clone.is_some() {
            if self
                .config
                .settings
                .mod_dirs
                .contains(&download_dir(&m.name))
            {
                2
            } else {
                0
            }
        } else {
            let (have, of) = installed_everywhere(self.installed.as_deref().unwrap_or(&[]), &m.id);
            if have == 0 {
                0
            } else if have == of {
                2
            } else {
                1
            }
        };
        let detail = [
            Some(m.marketplace.clone()),
            m.installs
                .map(|n| format!("{} installs", format_installs(n))),
            m.stars.map(|n| format!("{} stars", format_installs(n))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let label = match (m.clone.is_some(), busy, state) {
            (true, true, _) => "Downloading…",
            (true, false, _) => "Download",
            (false, true, _) => "Installing…",
            (false, false, 1) => "Add to all accounts",
            _ => "Install",
        };
        let url = m.url.clone();
        let pick = m.clone();
        div()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .id(SharedString::from(format!("cat-name-{}", m.id)))
                                    .text_sm()
                                    .when_some(url, |d, url| {
                                        d.role(Role::Link)
                                            .aria_label(format!(
                                                "Open the repository of {}",
                                                m.name
                                            ))
                                            .cursor_pointer()
                                            .text_color(theme.link)
                                            .on_click(move |_, _, cx| cx.open_url(&url))
                                    })
                                    .child(m.name.clone()),
                            )
                            .when_some(m.version.clone(), |d, v| {
                                d.child(div().text_xs().text_color(theme.muted_foreground).child(v))
                            }),
                    )
                    .when(!m.description.is_empty(), |d| {
                        d.child(div().text_xs().truncate().child(m.description.clone()))
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    )
                    .when_some(self.errors.get(&m.id).cloned(), |d, e| {
                        d.child(div().text_xs().text_color(theme.danger).child(e))
                    }),
            )
            .map(|d| {
                if state == 2 {
                    d.child(
                        div()
                            .px_2()
                            .text_xs()
                            .rounded_md()
                            .bg(theme.secondary)
                            .child(if m.clone.is_some() {
                                "In mod folders"
                            } else {
                                "Installed"
                            }),
                    )
                } else {
                    d.child(
                        button(format!("install-{}", m.id), label, idle, cx)
                            .aria_label(format!("Install {}", m.name))
                            .when(idle, |d| {
                                d.on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.install(pick.clone(), cx)
                                    }),
                                )
                            }),
                    )
                }
            })
            .into_any_element()
    }

    fn marketplaces_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let idle = self.busy.is_none();
        let hint = |t: String| div().text_sm().text_color(theme.muted_foreground).child(t);
        let mut body: Vec<AnyElement> = Vec::new();
        match &self.marketplaces {
            None => body.push(hint("Asking claude…".into()).into_any_element()),
            Some(Err(e)) => body.push(
                div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(e.clone())
                    .into_any_element(),
            ),
            Some(Ok(list)) if list.marketplaces.is_empty() => {
                body.push(hint("No marketplace yet.".into()).into_any_element())
            }
            Some(Ok(list)) => {
                for m in &list.marketplaces {
                    let confirming = self.confirm_market.as_deref() == Some(m.name.as_str());
                    let (name, url) = (m.name.clone(), m.url.clone());
                    body.push(
                        div()
                            .py_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .id(SharedString::from(format!("market-{}", m.name)))
                                    .flex_1()
                                    .text_sm()
                                    .when_some(url, |d, url| {
                                        d.role(Role::Link)
                                            .cursor_pointer()
                                            .text_color(theme.link)
                                            .on_click(move |_, _, cx| cx.open_url(&url))
                                    })
                                    .child(m.name.clone()),
                            )
                            .when(list.total > 1, |d| {
                                d.child(div().text_xs().text_color(theme.muted_foreground).child(
                                    if m.accounts == list.total {
                                        format!("all {} accounts", list.total)
                                    } else {
                                        format!("{} of {} accounts", m.accounts, list.total)
                                    },
                                ))
                            })
                            .child(
                                button(
                                    format!("market-remove-{}", m.name),
                                    if confirming {
                                        "Click to remove"
                                    } else {
                                        "Remove"
                                    },
                                    idle,
                                    cx,
                                )
                                .when(confirming, |d| d.text_color(theme.danger))
                                .when(idle, |d| {
                                    d.on_click(cx.listener(move |this, _, _, cx| {
                                        if this.confirm_market.as_deref() != Some(name.as_str()) {
                                            this.confirm_market = Some(name.clone());
                                            return cx.notify();
                                        }
                                        this.confirm_market = None;
                                        let n = name.clone();
                                        this.run(
                                            "market-add".into(),
                                            move |m, d| failed_line(m.remove_marketplace(d, &n)),
                                            cx,
                                        )
                                    }))
                                }),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        let can_add = idle && !Self::text(&self.market_source, cx).is_empty();
        section(
            "Marketplaces",
            "Where Browse finds mods. Adding or removing one does it for every claude account."
                .into(),
            cx,
        )
        .children(body)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .children(self.market_source.as_ref().map(Input::new)),
                )
                .child(
                    button(
                        "market-add",
                        if self.busy.as_deref() == Some("market-add") {
                            "Working…"
                        } else {
                            "Add marketplace"
                        },
                        can_add,
                        cx,
                    )
                    .when(can_add, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| this.add_marketplace(cx)))
                    }),
                ),
        )
        .when_some(self.errors.get("market-add").cloned(), |d, e| {
            d.child(div().text_xs().text_color(theme.danger).child(e))
        })
        .into_any_element()
    }

    fn folders_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let dirs = self.config.settings.mod_dirs.clone();
        let full = dirs.len() >= MOD_DIRS_MAX;
        let rows = dirs.iter().map(|d| {
            let name = downloaded_mod_name(d).map(str::to_string);
            let said = name.as_ref().and_then(|n| self.pulled.get(n).cloned());
            let (gone, pull_name) = (d.clone(), name.clone());
            div()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .font_family(crate::terminal::mono(cx))
                        .child(d.clone()),
                )
                .when_some(said.clone().flatten(), |r, s| {
                    r.child(div().text_xs().text_color(theme.muted_foreground).child(s))
                })
                .when_some(pull_name, |r, n| {
                    let running = matches!(said, Some(None));
                    r.child(
                        button(
                            format!("pull-{n}"),
                            if running { "Updating…" } else { "Update" },
                            !running,
                            cx,
                        )
                        .when(!running, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| this.pull(n.clone(), cx)))
                        }),
                    )
                })
                .child(
                    button(format!("dir-remove-{d}"), "✕", true, cx)
                        .aria_label(format!("Remove {d}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let next: Vec<String> = this
                                .config
                                .settings
                                .mod_dirs
                                .iter()
                                .filter(|x| **x != gone)
                                .cloned()
                                .collect();
                            this.set_dirs(next, cx);
                        })),
                )
        });
        section(
            "Mod folders",
            "For a mod you are building or keep in a folder of your own. Every claude pane, in every account, loads it without installing it.".into(),
            cx,
        )
        .children(rows)
        .child(
            div().flex().child(
                button("dir-add", "Add folder…", !full, cx).when(!full, |d| {
                    d.on_click(cx.listener(|this, _, window, cx| this.pick_dir(window, cx)))
                }),
            ),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Pick the mod's own folder, the one that holds .claude-plugin/plugin.json. Passed to claude as CLAUDE_CODE_PLUGIN_DIRS; claude reloads the mod when its files change."),
        )
        .into_any_element()
    }
}

impl Render for ModsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        let theme = cx.theme().clone();
        let has_claude = !self.claude_accounts().is_empty();
        let notify = self.config.settings.notify_panes;
        div()
            .id("mods")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .bg(theme.background)
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(ModsViewEvent::Close);
                }
            }))
            .child(
                div()
                    .max_w(px(820.))
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("Mods"))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(
                                "Mods change claude's own screen: side panes, a band above the prompt, status lines, toasts, commands. Install one once and every claude account has it.",
                            )),
                    )
                    .child(
                        button("mods-refresh", if self.loading { "Refreshing…" } else { "↻ Refresh" }, !self.loading, cx)
                            .aria_label("Refresh")
                            .when(!self.loading, |d| {
                                d.on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh(cx);
                                    match this.source {
                                        Source::Marketplaces => this.market = None,
                                        Source::Claudemod => this.community = None,
                                    }
                                    this.load_catalog(cx);
                                }))
                            }),
                    )
                    .child(
                        div()
                            .id("mods-close")
                            .a11y_button("Back to panes")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .child("← Back to panes")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ModsViewEvent::Close))),
                    ),
            )
            .when(self.changed, |d| {
                d.child(
                    div()
                        .id("mods-changed")
                        .role(Role::Status)
                        .max_w(px(820.))
                        .p_2()
                        .rounded_md()
                        .bg(theme.secondary)
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .child(div().flex_1().child(format!(
                            "Running claude panes load mods when they start.{}",
                            self.restarted.map_or(String::new(), |n| format!(" Restarted {n}."))
                        )))
                        .child(
                            button("mods-restart", "Restart claude panes", true, cx).on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ModsRequest::RestartClaudePanes)),
                            ),
                        ),
                )
            })
            .child(group("Installed", cx))
            .map(|d| {
                if has_claude {
                    d.child(self.installed_section(cx))
                } else {
                    d.child(
                        section("No claude account yet", String::new(), cx).child(
                            div().flex().child(
                                button("mods-add-account", "Add account", true, cx).on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(ModsRequest::AddAccount)),
                                ),
                            ),
                        ),
                    )
                }
            })
            .when(has_claude, |d| {
                d.child(group("Browse", cx))
                    .child(self.browse_section(cx))
                    .child(self.marketplaces_section(cx))
            })
            .child(group("In development", cx))
            .child(self.folders_section(cx))
            .child(group("In the panes", cx))
            .child(
                section(
                    "Copy and notifications",
                    format!(
                        "What a mod asks the terminal for, Wraithgrid carries out. Text a mod copies lands on your clipboard. A mod's notification becomes a desktop notification for a pane you aren't looking at{}; claude only sends those once preferredNotifChannel in its settings is ghostty, iterm2 or kitty. Pictures a mod draws show as their text description.",
                        if notify { "" } else { " (pane notifications are off in Settings)" }
                    ),
                    cx,
                ),
            )
    }
}
