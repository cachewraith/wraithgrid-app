//! A pane's overlays (`PaneOverlays.tsx`): the amber banner on an agent's terminal whose
//! account has not signed in yet (`LoginBanner`), and the bar at the bottom of a terminal whose
//! process ended (`ExitedOverlay`).

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::config::Pane;

use super::icons::{glyph, small, z, Glyph};
use super::sidebar::pal;
use super::{BtnKind, Shell};
use crate::a11y::A11y as _;

/// "<command> exited with code N", "… was stopped by signal S", "… could not start", from the
/// terminal's end message; and the detail, for a start error.
fn exit_headline(command: &str, ended: &str) -> (String, Option<String>) {
    let code = ended
        .strip_prefix("Exited with code ")
        .map(|r| r.split([' ', '.']).next().unwrap_or(""));
    let signal = ended
        .strip_prefix("Stopped by signal ")
        .map(|r| r.trim_end_matches('.'));
    match (code, signal) {
        (Some(c), _) => (format!("{command} exited with code {c}"), None),
        (_, Some(s)) => (format!("{command} was stopped by signal {s}"), None),
        _ => (
            format!("{command} could not start"),
            Some(ended.to_string()),
        ),
    }
}

impl Shell {
    /// "<account> is not signed in yet": Run /login, Mark as signed in, ×. On any agent's
    /// terminal whose account has not signed in, until its process ends.
    pub(super) fn login_banner(
        &mut self,
        pane: &Pane,
        chat: bool,
        exited: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if chat || exited || pane.shell || self.banner_hidden.contains(&pane.id) {
            return None;
        }
        let account = pane
            .account_id
            .as_deref()
            .and_then(|id| self.config().account(id))?
            .clone();
        if account.signed_in {
            return None;
        }
        let p = pal(cx);
        let started = self.login_started.contains(&pane.id);
        let (run, mark, hide) = (pane.id.clone(), account.id.clone(), pane.id.clone());
        Some(
            div()
                .id(SharedString::from(format!("login-banner-{}", pane.id)))
                .absolute()
                .left(z(10.))
                .right(z(10.))
                .top(z(10.))
                .p(z(12.))
                .flex()
                .flex_wrap()
                .items_start()
                .gap(z(10.))
                .rounded(z(8.))
                .border_1()
                .border_color(p.warn.opacity(0.4))
                .bg(p.panel)
                .shadow_lg()
                .text_size(z(12.5))
                .text_color(p.mu)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(div().mt(px(1.)).child(glyph(Glyph::Warn, 16., p.warn, p.panel)))
                .child(
                    div()
                        .flex_1()
                        .min_w(z(180.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.warn)
                                .child(if started {
                                    "Finish signing in in your browser".to_string()
                                } else {
                                    format!("{} is not signed in yet", account.name)
                                }),
                        )
                        .child(if started {
                            "This pane picks up the session when you are done. Other panes are not affected.".to_string()
                        } else {
                            format!(
                                "Run /login in this pane. Sign-in finishes in your browser and stays in {}.",
                                account.config_dir
                            )
                        }),
                )
                .child(
                    div()
                        .ml_auto()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(z(6.))
                        .when(!started, |d| {
                            d.child(
                                super::ui_btn(
                                    SharedString::from(format!("run-login-{}", pane.id)),
                                    BtnKind::Primary,
                                    &p,
                                )
                                .a11y_button("Run /login")
                                .child("Run /login")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.host_for(&run).write(&run, "/login\r");
                                    this.login_started.push(run.clone());
                                    // Typing goes straight on to the process.
                                    this.focus_pane(&run.clone(), window, cx);
                                    cx.notify();
                                })),
                            )
                        })
                        .child(
                            super::ui_btn(
                                SharedString::from(format!("mark-signed-{}", pane.id)),
                                BtnKind::Ghost,
                                &p,
                            )
                            .a11y_button("Mark as signed in")
                            .child("Mark as signed in")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let id = mark.clone();
                                this.update(cx, |c| {
                                    if let Some(a) = c.accounts.iter_mut().find(|a| a.id == id) {
                                        a.signed_in = true;
                                    }
                                });
                            })),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("hide-login-{}", pane.id)))
                                .a11y_button("Hide banner")
                                .size(z(30.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(z(6.))
                                .cursor_pointer()
                                .hover(|s| s.bg(p.raise))
                                .child(small(Glyph::Close, p.mu, p.panel))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.banner_hidden.push(hide.clone());
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// "<command> exited with code N · 14:02" at the bottom of a terminal whose process ended,
    /// with Restart. The pane stays open.
    pub(super) fn exited_bar(
        &mut self,
        pane: &Pane,
        command: &str,
        ended: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let message = ended?;
        let p = pal(cx);
        let at = *self
            .ended_at
            .entry(pane.id.clone())
            .or_insert_with(wraithgrid_usage::now_ms);
        let (headline, detail) = exit_headline(command, message);
        let restart = pane.id.clone();
        Some(
            div()
                .id(SharedString::from(format!("exited-{}", pane.id)))
                .role(Role::Status)
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .px(z(14.))
                .py(z(12.))
                .flex()
                .items_center()
                .gap(z(12.))
                .bg(p.raise)
                .border_t_1()
                .border_color(p.line2)
                .text_size(z(12.5))
                .text_color(p.mu)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(glyph(Glyph::Error, 16., p.err, p.raise))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.tx)
                                .child(headline),
                        )
                        .child(div().flex_none().child(format!(
                            "\u{a0}· {}",
                            wraithgrid_usage::format::local_hhmm(at)
                        )))
                        .when_some(detail, |d, m| {
                            d.child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .font_family(crate::terminal::mono(cx))
                                    .child(format!("\u{a0}· {m}")),
                            )
                        }),
                )
                .child(
                    super::ui_btn(
                        SharedString::from(format!("restart-bar-{}", pane.id)),
                        BtnKind::Primary,
                        &p,
                    )
                    .a11y_button("Restart")
                    .flex_none()
                    .child(glyph(Glyph::Restart, 16., cx_fg(cx), p.acc))
                    .child("Restart")
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.restart_pane(restart.clone(), cx)),
                    ),
                )
                .into_any_element(),
        )
    }
}

/// The text colour on an accent-filled button.
fn cx_fg(cx: &App) -> Hsla {
    use gpui_kit::component::ActiveTheme as _;
    cx.theme().primary_foreground
}

#[cfg(test)]
mod tests {
    use super::exit_headline;

    #[test]
    fn exit_headlines() {
        assert_eq!(
            exit_headline("claude", "Exited with code 1."),
            ("claude exited with code 1".into(), None)
        );
        assert_eq!(
            exit_headline("shell", "Exited with code 0 while Wraithgrid was closed."),
            ("shell exited with code 0".into(), None)
        );
        assert_eq!(
            exit_headline("codex", "Stopped by signal 9."),
            ("codex was stopped by signal 9".into(), None)
        );
        assert_eq!(
            exit_headline("gemini", "gemini not found on PATH"),
            (
                "gemini could not start".into(),
                Some("gemini not found on PATH".into())
            )
        );
    }
}
