//! The account chip under the chat (`AccountPicker` in ModelPicker.tsx, #160): "Continue as"
//! another account. Providers on a rail at the left, the chosen one's accounts on the right
//! (current first) with their 5-hour use; typing searches every provider, grouped. ↑↓ move,
//! Enter chooses, Tab / Shift+Tab change provider, Esc closes.

use gpui_kit::component::input::{self, Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::agent_cli::AgentCli;

use super::{ChatView, ChatViewEvent};

/// One account the chat can carry on as.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountChoice {
    pub id: String,
    pub cli: String,
    pub name: String,
    /// Five-hour limit used, in %, when the provider has reported one.
    pub session_pct: Option<i64>,
    pub signed_in: bool,
}

pub(super) struct AccountPicker {
    query: Entity<InputState>,
    tab: String,
    active: usize,
    _sub: Subscription,
}

const ORDER: [&str; 7] = [
    "claude", "codex", "agy", "gemini", "cursor", "copilot", "opencode",
];

fn provider(cli: &str) -> &'static str {
    AgentCli::parse(cli).map_or("", |c| c.spec().provider_label)
}

/// What switching to an account of `cli` does, from a chat of `from`.
fn switch_hint(cli: &str, from: &str, short: bool) -> &'static str {
    if cli == from {
        if short {
            "same chat"
        } else {
            "continue · same chat"
        }
    } else if AgentCli::parse(cli).is_some_and(|c| c.spec().chat) {
        if short {
            "new chat"
        } else {
            "new chat · handoff"
        }
    } else if short {
        "terminal"
    } else {
        "terminal · no handoff"
    }
}

impl ChatView {
    /// The accounts the chip offers, and which one the chat runs as.
    pub fn set_accounts(
        &mut self,
        current: Option<String>,
        choices: Vec<AccountChoice>,
        cx: &mut Context<Self>,
    ) {
        if self.current_account != current || self.account_choices != choices {
            self.current_account = current;
            self.account_choices = choices;
            cx.notify();
        }
    }

    fn current_choice(&self) -> Option<&AccountChoice> {
        let id = self.current_account.as_ref()?;
        self.account_choices.iter().find(|a| &a.id == id)
    }

    fn toggle_account_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account_picker.take().is_some() {
            return cx.notify();
        }
        let Some(cur) = self.current_choice().cloned() else {
            return;
        };
        self.picker = None;
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search accounts"));
        let sub = cx.subscribe(&query, |this, _, e: &InputEvent, cx| {
            if let InputEvent::Change = e {
                if let Some(p) = &mut this.account_picker {
                    p.active = 0;
                }
                cx.notify();
            }
        });
        window.focus(&query.focus_handle(cx), cx);
        self.account_picker = Some(AccountPicker {
            query,
            tab: cur.cli,
            active: 0,
            _sub: sub,
        });
        cx.notify();
    }

    fn close_account_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.account_picker = None;
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    /// The providers shown (current first in ORDER), and the accounts listed now.
    fn account_view(&self, cx: &App) -> (Vec<String>, Vec<AccountChoice>, String) {
        let Some(cur) = self.current_choice() else {
            return (vec![], vec![], String::new());
        };
        let Some(p) = &self.account_picker else {
            return (vec![], vec![], String::new());
        };
        let q = p.query.read(cx).value().trim().to_lowercase();
        let of = |cli: &str| -> Vec<AccountChoice> {
            self.account_choices
                .iter()
                .filter(|a| a.cli == cli)
                .cloned()
                .collect()
        };
        let tabs: Vec<String> = ORDER
            .iter()
            .filter(|cli| **cli == cur.cli || !of(cli).is_empty())
            .map(|s| s.to_string())
            .collect();
        let matches = |a: &AccountChoice| {
            format!("{} {}", a.name, provider(&a.cli))
                .to_lowercase()
                .contains(&q)
        };
        let shown: Vec<AccountChoice> = if q.is_empty() {
            let mut v = of(&p.tab);
            v.sort_by_key(|a| a.id != cur.id);
            v
        } else {
            tabs.iter()
                .flat_map(|t| of(t).into_iter().filter(matches))
                .collect()
        };
        (tabs, shown, q)
    }

    fn pick_account(&mut self, a: &AccountChoice, window: &mut Window, cx: &mut Context<Self>) {
        let busy = self.thread.busy;
        let current = self.current_account.clone();
        if busy && current.as_ref() != Some(&a.id) {
            return;
        }
        self.close_account_picker(window, cx);
        if current.as_ref() == Some(&a.id) {
            return;
        }
        cx.emit(if a.signed_in {
            ChatViewEvent::ContinueAs(a.id.clone())
        } else {
            ChatViewEvent::SignIn(a.id.clone())
        });
    }

    fn account_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (_, shown, _) = self.account_view(cx);
        let Some(p) = &mut self.account_picker else {
            return;
        };
        let n = shown.len().max(1);
        match e.keystroke.key.as_str() {
            "escape" => self.close_account_picker(window, cx),
            "down" => p.active = (p.active + 1) % n,
            "up" => p.active = (p.active + n - 1) % n,
            "enter" => {
                if let Some(a) = shown.get(p.active).cloned() {
                    self.pick_account(&a, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Tab / Shift+Tab: the next or previous provider. The search box binds them as actions.
    fn account_tab(&mut self, step: isize, cx: &mut Context<Self>) {
        let (tabs, _, q) = self.account_view(cx);
        let Some(p) = &mut self.account_picker else {
            return;
        };
        if q.is_empty() && tabs.len() > 1 {
            let at = tabs.iter().position(|t| *t == p.tab).unwrap_or(0) as isize;
            let len = tabs.len() as isize;
            p.tab = tabs[((at + step + len) % len) as usize].clone();
            p.active = 0;
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The chip, first in the row under the thread.
    pub(super) fn account_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cur = self.current_choice()?.clone();
        let theme = cx.theme();
        let open = self.account_picker.is_some();
        Some(
            div()
                .id("pick-account")
                .role(Role::Button)
                .aria_label(format!(
                    "Account: {}. Continue as another account",
                    cur.name
                ))
                .aria_expanded(open)
                .px_2()
                .py(px(1.))
                .flex()
                .items_center()
                .gap_1()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .text_color(theme.muted_foreground)
                .hover(|d| d.bg(theme.secondary))
                .when(open, |d| d.bg(theme.secondary))
                .child(crate::shell::provider_mark(&cur.cli, &cur.name, 12., cx))
                .child(cur.name.clone())
                .on_click(cx.listener(|this, _, window, cx| this.toggle_account_picker(window, cx)))
                .into_any_element(),
        )
    }

    pub(super) fn account_list(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.account_picker.as_ref()?;
        let cur = self.current_choice()?.clone();
        let (tabs, shown, q) = self.account_view(cx);
        let theme = cx.theme().clone();
        let busy = self.thread.busy;
        let row = |i: usize, a: &AccountChoice, cx: &mut Context<Self>| {
            let on = a.id == cur.id;
            let active = i == p.active;
            let pick = a.clone();
            let disabled = busy && !on;
            div()
                .id(SharedString::from(format!("acct-opt-{}", a.id)))
                .role(Role::ListItem)
                .aria_label(a.name.clone())
                .aria_selected(on)
                .when(active, |d| d.aria_active_descendant())
                .h(px(30.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .text_sm()
                .when(active, |d| d.bg(theme.list_active))
                .when(disabled, |d| d.opacity(0.5))
                .when(!disabled, |d| d.cursor_pointer())
                .child(crate::shell::provider_mark(&a.cli, &a.name, 14., cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .gap_2()
                        .items_baseline()
                        .child(div().truncate().child(a.name.clone()))
                        .when(!a.signed_in, |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Not signed in"),
                            )
                        }),
                )
                .child(match (a.signed_in, a.session_pct) {
                    (false, _) => div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Sign in")
                        .into_any_element(),
                    (true, None) => div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("—")
                        .into_any_element(),
                    (true, Some(pct)) => div()
                        .flex_none()
                        .w(px(48.))
                        .h(px(4.))
                        .rounded_full()
                        .bg(theme.border)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(if pct >= 80 {
                                    theme.warning
                                } else {
                                    theme.primary
                                })
                                .w(relative(pct.clamp(0, 100) as f32 / 100.)),
                        )
                        .into_any_element(),
                })
                .child(div().w(px(12.)).when(on, |d| d.child("✓")))
                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                    if let Some(p) = &mut this.account_picker {
                        if p.active != i {
                            p.active = i;
                            cx.notify();
                        }
                    }
                }))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.pick_account(&pick, window, cx)),
                )
                .into_any_element()
        };
        let mut list: Vec<AnyElement> = vec![];
        if q.is_empty() {
            for (i, a) in shown.iter().enumerate() {
                list.push(row(i, a, cx));
            }
        } else {
            let mut i = 0;
            for t in &tabs {
                let group: Vec<_> = shown.iter().filter(|a| &a.cli == t).collect();
                if group.is_empty() {
                    continue;
                }
                list.push(
                    div()
                        .px_2()
                        .pt_1()
                        .flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(if t == "codex" {
                            "ChatGPT · Codex"
                        } else {
                            provider(t)
                        })
                        .child(switch_hint(t, &cur.cli, false))
                        .into_any_element(),
                );
                for a in group {
                    list.push(row(i, a, cx));
                    i += 1;
                }
            }
        }
        if shown.is_empty() {
            list.push(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No accounts found.")
                    .into_any_element(),
            );
        }
        let rail = (tabs.len() > 1).then(|| {
            div()
                .id("acct-rail")
                .role(Role::TabList)
                .aria_label("Providers")
                .w(px(112.))
                .flex_none()
                .flex()
                .flex_col()
                .gap_0p5()
                .pr_1()
                .border_r_1()
                .border_color(theme.border)
                .children(tabs.iter().map(|t| {
                    let n = self
                        .account_choices
                        .iter()
                        .filter(|a| &a.cli == t)
                        .filter(|a| {
                            q.is_empty()
                                || format!("{} {}", a.name, provider(&a.cli))
                                    .to_lowercase()
                                    .contains(&q)
                        })
                        .count();
                    let on = q.is_empty() && *t == p.tab;
                    let tab = t.clone();
                    div()
                        .id(SharedString::from(format!("acct-tab-{t}")))
                        .role(Role::Tab)
                        .aria_label(provider(t))
                        .aria_selected(on)
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(on, |d| d.bg(theme.secondary))
                        .when(!q.is_empty() && n == 0, |d| d.opacity(0.5))
                        .hover(|d| d.bg(theme.secondary))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_sm()
                                .child(crate::shell::provider_mark(t, t, 12., cx))
                                .child(div().flex_1().truncate().child(provider(t)))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(n.to_string()),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(switch_hint(t, &cur.cli, true)),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(p) = &mut this.account_picker {
                                p.tab = tab.clone();
                                p.active = 0;
                                p.query.update(cx, |q, cx| q.set_value("", window, cx));
                            }
                            cx.notify();
                        }))
                }))
        });
        let width = if tabs.len() > 1 { 460. } else { 360. };
        Some(
            div()
                .id("acct-picker")
                .role(Role::Dialog)
                .aria_label("Continue as")
                .mx_2()
                // As wide as the provider rail needs, never wider than the pane.
                .max_w(px(width))
                .min_w_0()
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .flex()
                .flex_col()
                .gap_1()
                .capture_key_down(cx.listener(Self::account_key))
                .capture_action(
                    cx.listener(|this, _: &input::IndentInline, _, cx| this.account_tab(1, cx)),
                )
                .capture_action(
                    cx.listener(|this, _: &input::OutdentInline, _, cx| this.account_tab(-1, cx)),
                )
                .child(Input::new(&p.query))
                .when(busy, |d| {
                    d.child(
                        div()
                            .px_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Wait for the answer to finish, or stop it first."),
                    )
                })
                .child(
                    div().flex().gap_1().max_h(px(300.)).children(rail).child(
                        div()
                            .id("acct-list")
                            .role(Role::List)
                            .aria_label("Accounts")
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .children(list),
                    ),
                )
                .child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "↑↓ move · ↵ choose{} · esc close",
                            if tabs.len() > 1 {
                                " · tab provider"
                            } else {
                                ""
                            }
                        )),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn says_what_a_switch_does() {
        assert_eq!(switch_hint("claude", "claude", true), "same chat");
        assert_eq!(switch_hint("codex", "claude", false), "new chat · handoff");
        assert_eq!(switch_hint("cursor", "claude", true), "terminal");
    }
}
