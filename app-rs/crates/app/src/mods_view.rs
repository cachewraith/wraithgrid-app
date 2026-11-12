//! Mods (`ModsView.tsx`): each claude account's installed mods (claude plugins) to turn on
//! and off, and the community list to install from. The work is the `wraithgrid-mods` crate's
//! `ModManager`, run off the UI thread through each account's own `claude plugin` commands.

use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::config::Config;
use wraithgrid_mods::mods::ClaudeCli;
use wraithgrid_mods::{AccountMods, CatalogMod, CommunityCatalog, ModDeps, ModManager};

pub enum ModsViewEvent {
    Close,
}

pub struct ModsView {
    focus: FocusHandle,
    config: Config,
    manager: Arc<ModManager>,
    installed: Option<Vec<AccountMods>>,
    community: Option<Result<Vec<CatalogMod>, String>>,
    busy: Option<String>,
    note: Option<(bool, String)>,
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

impl ModsView {
    pub fn new(config: Config, cx: &mut Context<Self>) -> Self {
        let manager = Arc::new(ModManager::new(
            wraithgrid_core::paths::home_dir(),
            CommunityCatalog::new(wraithgrid_mods::mod_community::ureq_fetch),
        ));
        let mut view = ModsView {
            focus: cx.focus_handle(),
            config,
            manager,
            installed: None,
            community: None,
            busy: None,
            note: None,
        };
        view.refresh(cx);
        view
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (config, manager) = (self.config.clone(), self.manager.clone());
        cx.spawn(async move |this, cx| {
            let (installed, community) = cx
                .background_spawn(async move {
                    let installed = with_deps(&config, |d| manager.list(d));
                    (installed, manager.community())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.installed = Some(installed);
                this.community = Some(community);
                cx.notify();
            });
        })
        .detach();
    }

    /// One mod change in the background, then the lists again.
    fn run(
        &mut self,
        what: String,
        job: impl FnOnce(&ModManager, &ModDeps<'_>) -> Result<(), String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(what.clone());
        let (config, manager) = (self.config.clone(), self.manager.clone());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { with_deps(&config, |d| job(&manager, d)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.note = Some(match result {
                    Ok(()) => (
                        true,
                        format!("{what}: done. Restart claude panes to pick it up."),
                    ),
                    Err(e) => (false, e),
                });
                this.refresh(cx);
            });
        })
        .detach();
        cx.notify();
    }
}

impl EventEmitter<ModsViewEvent> for ModsView {}

impl Focusable for ModsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ModsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let small = |id: String, label: String| {
            div()
                .id(SharedString::from(id))
                .px_2()
                .py(px(2.))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_xs()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(label)
        };
        let name_of = |id: &str| {
            self.config
                .account(id)
                .map_or_else(|| id.to_string(), |a| a.name.clone())
        };
        let installed: Vec<AnyElement> = match &self.installed {
            None => vec![div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Asking claude for each account's mods…")
                .into_any_element()],
            Some(list) if list.is_empty() => vec![div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No claude accounts yet.")
                .into_any_element()],
            Some(list) => list
                .iter()
                .map(|acct| {
                    let rows: Vec<AnyElement> = match &acct.mods {
                        Err(e) => vec![div()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(e.clone())
                            .into_any_element()],
                        Ok(mods) if mods.is_empty() => vec![div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No mods installed.")
                            .into_any_element()],
                        Ok(mods) => mods
                            .iter()
                            .map(|m| {
                                let (account, id, scope, path, on) = (
                                    acct.account_id.clone(),
                                    m.id.clone(),
                                    m.scope.clone(),
                                    m.project_path.clone(),
                                    m.enabled,
                                );
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_sm()
                                    .child(div().flex_1().truncate().child(m.id.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{} · {}", m.version, m.scope)),
                                    )
                                    .child(
                                        small(
                                            format!("toggle-{}-{}", acct.account_id, m.id),
                                            if on {
                                                "Turn off".into()
                                            } else {
                                                "Turn on".into()
                                            },
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                let (account, id, scope, path) = (
                                                    account.clone(),
                                                    id.clone(),
                                                    scope.clone(),
                                                    path.clone(),
                                                );
                                                let what = format!(
                                                    "{} {id}",
                                                    if on { "Turning off" } else { "Turning on" }
                                                );
                                                this.run(
                                                    what,
                                                    move |m, d| {
                                                        m.set_enabled(
                                                            d,
                                                            &account,
                                                            &id,
                                                            &scope,
                                                            path.as_deref(),
                                                            !on,
                                                        )
                                                    },
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                    .into_any_element()
                            })
                            .collect(),
                    };
                    div()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(name_of(&acct.account_id)),
                        )
                        .children(rows)
                        .into_any_element()
                })
                .collect(),
        };
        let community: Vec<AnyElement> = match &self.community {
            None => vec![div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Loading the community list…")
                .into_any_element()],
            Some(Err(e)) => vec![div()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element()],
            Some(Ok(list)) => list
                .iter()
                .take(60)
                .map(|m| {
                    let id = m.id.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .py_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(div().text_sm().child(m.name.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .truncate()
                                        .text_color(theme.muted_foreground)
                                        .child(m.description.clone()),
                                ),
                        )
                        .when_some(m.stars, |d, s| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("★ {s}")),
                            )
                        })
                        .when(m.clone.is_none(), |d| {
                            d.child(
                                small(format!("install-{}", m.id), "Install".into()).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        let id = id.clone();
                                        this.run(
                                            format!("Installing {id}"),
                                            move |m, d| m.install(d, &id).map(|_| ()),
                                            cx,
                                        );
                                    }),
                                ),
                            )
                        })
                        .into_any_element()
                })
                .collect(),
        };
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
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Mods"),
                    )
                    .child(
                        div()
                            .id("mods-close")
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
            .when_some(self.busy.clone(), |d, b| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!("{b}…")),
                )
            })
            .when_some(self.note.clone(), |d, (ok, n)| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(if ok { theme.success } else { theme.danger })
                        .child(n),
                )
            })
            .child(
                div()
                    .max_w(px(820.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Installed"))
                    .children(installed),
            )
            .child(
                div()
                    .max_w(px(820.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .pb_1()
                            .child("Community"),
                    )
                    .children(community),
            )
    }
}
