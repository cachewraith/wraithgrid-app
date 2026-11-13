//! Accounts (`AccountsView.tsx`): the accounts with their CLI and config dir, adding one,
//! renaming, removing, and signing in. The view says what the user asked for; the window
//! changes the config and reports errors back.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::a11y::A11y as _;
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::config::Config;
use wraithgrid_core::types::ACCOUNT_COLORS;

mod folders;
pub use folders::FolderOp;

pub enum AccountsEvent {
    Add {
        name: String,
        cli: AgentCli,
        color: String,
    },
    Rename {
        id: String,
        name: String,
    },
    Remove {
        id: String,
    },
    Login {
        id: String,
    },
    MarkSignedIn {
        id: String,
    },
    /// Account folders (`accountFolders`) and which folder an account is in.
    Folder(FolderOp),
    Close,
}

struct Row {
    id: String,
    name: String,
    cli: AgentCli,
    config_dir: String,
    color: Hsla,
    signed_in: bool,
    rename: Entity<InputState>,
}

pub struct AccountsView {
    focus: FocusHandle,
    rows: Vec<Row>,
    name: Entity<InputState>,
    cli: AgentCli,
    color: String,
    confirm_remove: Option<String>,
    pub error: Option<String>,
    folders: folders::Folders,
    _subscriptions: Vec<Subscription>,
}

impl AccountsView {
    pub fn new(config: &Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. work"));
        let mut subs = vec![
            cx.subscribe_in(&name, window, |this, _, e: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    this.add(cx);
                }
            }),
        ];
        let mut rows = Vec::new();
        for a in &config.accounts {
            let rename = cx.new(|cx| InputState::new(window, cx).default_value(a.name.clone()));
            let id = a.id.clone();
            subs.push(
                cx.subscribe_in(&rename, window, move |_, input, e: &InputEvent, _, cx| {
                    // Saved on Enter or when the field loses focus.
                    if matches!(e, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let name = input.read(cx).value().to_string();
                        cx.emit(AccountsEvent::Rename {
                            id: id.clone(),
                            name,
                        });
                    }
                }),
            );
            rows.push(Row {
                id: a.id.clone(),
                name: a.name.clone(),
                cli: a.cli,
                config_dir: a.config_dir.clone(),
                color: crate::shell::parse_hex(&a.color).unwrap_or(rgb(0x7c5cff).into()),
                signed_in: a.signed_in,
                rename,
            });
        }
        AccountsView {
            focus: cx.focus_handle(),
            rows,
            name,
            cli: AgentCli::Claude,
            color: wraithgrid_core::accounts::next_free_color(&config.accounts),
            confirm_remove: None,
            error: None,
            folders: folders::Folders::new(config),
            _subscriptions: subs,
        }
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().to_string();
        cx.emit(AccountsEvent::Add {
            name,
            cli: self.cli,
            color: self.color.clone(),
        });
    }
}

impl EventEmitter<AccountsEvent> for AccountsView {}

impl Focusable for AccountsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AccountsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let small = |id: SharedString, label: &'static str| {
            div()
                .id(id)
                .a11y_button(label)
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
        let folders = (!self.rows.is_empty()).then(|| self.folders_section(window, cx));
        let rows = self.rows.iter().map(|r| {
            let (login, mark, remove, really) =
                (r.id.clone(), r.id.clone(), r.id.clone(), r.id.clone());
            let confirming = self.confirm_remove.as_ref() == Some(&r.id);
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().size(px(10.)).rounded_full().bg(r.color))
                        .child(div().w(px(220.)).child(Input::new(&r.rename)))
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(r.cli.spec().label),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_xs()
                                .text_color(if r.signed_in {
                                    theme.success
                                } else {
                                    theme.warning
                                })
                                .child(if r.signed_in {
                                    "signed in"
                                } else {
                                    "not signed in"
                                }),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(crate::terminal::mono(cx))
                        .text_color(theme.muted_foreground)
                        .child(r.config_dir.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            small(SharedString::from(format!("login-{}", r.id)), "Login").on_click(
                                cx.listener(move |_, _, _, cx| {
                                    cx.emit(AccountsEvent::Login { id: login.clone() })
                                }),
                            ),
                        )
                        .when(!r.signed_in, |d| {
                            d.child(
                                small(
                                    SharedString::from(format!("mark-{}", r.id)),
                                    "Mark as signed in",
                                )
                                .on_click(cx.listener(
                                    move |_, _, _, cx| {
                                        cx.emit(AccountsEvent::MarkSignedIn { id: mark.clone() })
                                    },
                                )),
                            )
                        })
                        .child(div().flex_1())
                        .when(!confirming, |d| {
                            d.child(
                                small(SharedString::from(format!("remove-{}", r.id)), "Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.confirm_remove = Some(remove.clone());
                                        cx.notify();
                                    })),
                            )
                        })
                        .when(confirming, |d| {
                            d.child(div().text_xs().text_color(theme.danger).child(format!(
                                "Remove {}? Its panes close; its folder stays.",
                                r.name
                            )))
                            .child(
                                small(SharedString::from(format!("really-{}", r.id)), "Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.confirm_remove = None;
                                        cx.emit(AccountsEvent::Remove { id: really.clone() });
                                    })),
                            )
                            .child(
                                small(SharedString::from(format!("keep-{}", r.id)), "Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_remove = None;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
        });
        let clis = AgentCli::ALL.iter().map(|cli| {
            let cli = *cli;
            let on = self.cli == cli;
            div()
                .id(SharedString::from(format!("cli-{cli}")))
                .a11y_toggle(cli.spec().provider_label, on)
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .text_sm()
                .cursor_pointer()
                .border_color(if on { theme.ring } else { theme.border })
                .when(on, |d| d.bg(theme.secondary))
                .child(cli.spec().provider_label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.cli = cli;
                    cx.notify();
                }))
        });
        let colors = ACCOUNT_COLORS.iter().map(|(name, value)| {
            let value = value.to_string();
            let on = self.color.eq_ignore_ascii_case(&value);
            div()
                .id(SharedString::from(format!("color-{name}")))
                .a11y_toggle(format!("Color {name}"), on)
                .size(px(22.))
                .rounded_full()
                .cursor_pointer()
                .border_2()
                .border_color(if on {
                    theme.foreground
                } else {
                    theme.background
                })
                .bg(crate::shell::parse_hex(&value).unwrap_or(theme.primary))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.color = value.clone();
                    cx.notify();
                }))
        });
        div()
            .id("accounts")
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
                    cx.emit(AccountsEvent::Close);
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
                            .child("Accounts"),
                    )
                    .child(
                        div()
                            .id("accounts-close")
                            .a11y_button("Back to panes")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .child("← Back to panes")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::Close))),
                    ),
            )
            .children(folders)
            .child(
                div()
                    .max_w(px(760.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(rows),
            )
            .child(
                div()
                    .max_w(px(760.))
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Add an account"),
                    )
                    .child(Input::new(&self.name))
                    .child(div().flex().flex_wrap().gap_2().children(clis))
                    .child(div().flex().gap_2().children(colors))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(self.cli.spec().note),
                    )
                    .when_some(self.error.clone(), |d, e| {
                        d.child(div().text_sm().text_color(theme.danger).child(e))
                    })
                    .child(
                        div().flex().justify_end().child(
                            div()
                                .id("add-account")
                                .a11y_button("Add account")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(theme.primary)
                                .text_color(theme.primary_foreground)
                                .cursor_pointer()
                                .child("Add account")
                                .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                        ),
                    ),
            )
    }
}
