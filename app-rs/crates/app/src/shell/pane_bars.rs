//! A pane's bars (`PaneOverlays.tsx`): the amber one on a claude terminal whose account has not
//! signed in yet (`LoginBanner`), and the one at the bottom of a terminal whose process ended
//! (`ExitedOverlay`).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::agent_cli::AgentCli;
use wraithgrid_core::config::Pane;

use super::Shell;
use crate::a11y::A11y as _;

impl Shell {
    /// "<account> is not signed in yet": Run /login, Mark as signed in, ×.
    pub(super) fn login_banner(
        &mut self,
        pane: &Pane,
        chat: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if chat || pane.shell || self.banner_hidden.contains(&pane.id) {
            return None;
        }
        let account = pane
            .account_id
            .as_deref()
            .and_then(|id| self.config().account(id))?
            .clone();
        if account.signed_in || account.cli != AgentCli::Claude {
            return None;
        }
        let theme = cx.theme().clone();
        let started = self.login_started.contains(&pane.id);
        let (run, mark, hide) = (pane.id.clone(), account.id.clone(), pane.id.clone());
        let btn = |id: String, label: &'static str| {
            div()
                .id(SharedString::from(id))
                .a11y_button(label)
                .px_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(label)
        };
        Some(
            div()
                .px_3()
                .py_2()
                .flex()
                .items_start()
                .gap_2()
                .text_xs()
                .bg(theme.warning.opacity(0.12))
                .border_b_1()
                .border_color(theme.warning.opacity(0.4))
                .child(div().text_color(theme.warning).child("⚠"))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(if started {
                            "Finish signing in in your browser".to_string()
                        } else {
                            format!("{} is not signed in yet", account.name)
                        }))
                        .child(if started {
                            "This pane picks up the session when you are done. Other panes are not affected.".to_string()
                        } else {
                            format!(
                                "Run /login in this pane. Sign-in finishes in your browser and stays in {}.",
                                account.config_dir
                            )
                        }),
                )
                .when(!started, |d| {
                    d.child(btn(format!("run-login-{}", pane.id), "Run /login").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.host_for(&run).write(&run, "/login\r");
                            this.login_started.push(run.clone());
                            cx.notify();
                        },
                    )))
                })
                .child(btn(format!("mark-signed-{}", pane.id), "Mark as signed in").on_click(cx.listener(
                    move |this, _, _, cx| {
                        let id = mark.clone();
                        this.update(cx, |c| {
                            if let Some(a) = c.accounts.iter_mut().find(|a| a.id == id) {
                                a.signed_in = true;
                            }
                        });
                    },
                )))
                .child(
                    div()
                        .id(SharedString::from(format!("hide-login-{}", pane.id)))
                        .a11y_button("Dismiss")
                        .px_1()
                        .cursor_pointer()
                        .child("×")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.banner_hidden.push(hide.clone());
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// "<command> exited …" at the bottom of a terminal whose process ended, with Restart.
    pub(super) fn exited_bar(
        &mut self,
        pane: &Pane,
        ended: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let message = ended?;
        let theme = cx.theme().clone();
        let restart = pane.id.clone();
        Some(
            div()
                .px_3()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .bg(theme.danger.opacity(0.1))
                .border_t_1()
                .border_color(theme.danger.opacity(0.35))
                .child(div().text_color(theme.danger).child("●"))
                .child(div().flex_1().truncate().child(message.to_string()))
                .child(
                    div()
                        .id(SharedString::from(format!("restart-bar-{}", pane.id)))
                        .a11y_button("Restart")
                        .px_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.secondary))
                        .child("↻ Restart")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.restart_pane(restart.clone(), cx)
                        })),
                )
                .into_any_element(),
        )
    }
}
