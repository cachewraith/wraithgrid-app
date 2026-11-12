//! Settings → Advanced → Machines (`MachinesRow` in SettingsView.tsx, H5): the computers a
//! project's panes can run on over ssh. Add (an ssh host from `~/.ssh/config` or user@host,
//! and a name), Test (connects, starting Wraithgrid's host there if needed) and Remove.

use std::collections::HashMap;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::config::{is_ssh_host, Machine};
use wraithgrid_core::ids::new_id;

pub enum MachinesEvent {
    Add(Machine),
    /// Its projects run here again; its panes there keep running.
    Remove(String),
}

pub struct MachinesRow {
    machines: Vec<Machine>,
    /// Host names from `~/.ssh/config`, offered while typing.
    hosts: Vec<String>,
    ssh_host: Entity<InputState>,
    name: Entity<InputState>,
    error: Option<String>,
    tests: HashMap<String, String>,
    _subscriptions: Vec<Subscription>,
}

impl MachinesRow {
    pub fn new(machines: Vec<Machine>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ssh_host = cx.new(|cx| {
            InputState::new(window, cx).placeholder("ssh host (from ~/.ssh/config) or user@host")
        });
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name (optional)"));
        let subs = vec![
            cx.observe(&ssh_host, |_, _, cx| cx.notify()),
            cx.subscribe_in(&name, window, |this, _, e: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    this.add(window, cx);
                }
            }),
        ];
        cx.spawn(async move |this, cx| {
            let hosts = cx
                .background_spawn(async {
                    let home = wraithgrid_core::paths::home_dir();
                    std::fs::read_to_string(home.join(".ssh").join("config"))
                        .map(|t| wraithgrid_remote::ssh_config_hosts(&t))
                        .unwrap_or_default()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.hosts = hosts;
                cx.notify();
            });
        })
        .detach();
        MachinesRow {
            machines,
            hosts: vec![],
            ssh_host,
            name,
            error: None,
            tests: HashMap::new(),
            _subscriptions: subs,
        }
    }

    pub fn set_machines(&mut self, machines: Vec<Machine>, cx: &mut Context<Self>) {
        self.machines = machines;
        cx.notify();
    }

    /// `addMachine`: the error to show, or the machine is added and the fields clear.
    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.ssh_host.read(cx).value().trim().to_string();
        let typed = self.name.read(cx).value().trim().to_string();
        let name: String = (if typed.is_empty() { &host } else { &typed })
            .chars()
            .take(64)
            .collect();
        self.error = if name.is_empty() {
            Some("Give the machine a name.".into())
        } else if !is_ssh_host(&host) || host.len() > 255 {
            Some(
                "Use a Host from ~/.ssh/config or user@host (no spaces, not starting with -)."
                    .into(),
            )
        } else if self
            .machines
            .iter()
            .any(|m| m.name.to_lowercase() == name.to_lowercase())
        {
            Some(format!("A machine named {name} exists."))
        } else {
            None
        };
        if self.error.is_none() {
            cx.emit(MachinesEvent::Add(Machine {
                id: new_id("m"),
                name,
                ssh_host: host,
            }));
            self.ssh_host
                .update(cx, |i, cx| i.set_value("", window, cx));
            self.name.update(cx, |i, cx| i.set_value("", window, cx));
        }
        cx.notify();
    }

    fn test(&mut self, m: Machine, cx: &mut Context<Self>) {
        self.tests.insert(m.id.clone(), "Connecting…".into());
        let id = m.id.clone();
        cx.spawn(async move |this, cx| {
            let text = cx
                .background_spawn(async move {
                    // Its own link, as `ptys.probe` makes one: nothing is routed from it.
                    let probe = Machine {
                        id: "probe".into(),
                        ..m
                    };
                    let host = crate::shell::remote_host(&probe, None);
                    let text = crate::shell::probe_text(host.probe());
                    host.detach();
                    text
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.tests.insert(id, text);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl EventEmitter<MachinesEvent> for MachinesRow {}

impl Render for MachinesRow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let button = |id: String, label: &'static str| {
            div()
                .id(SharedString::from(id))
                .px_2()
                .py(px(2.))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_sm()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(label)
        };
        let rows = self.machines.iter().map(|m| {
            let (test, remove) = (m.clone(), m.id.clone());
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(m.name.clone()),
                        )
                        .child(
                            div()
                                .font_family(crate::terminal::mono(cx))
                                .child(m.ssh_host.clone()),
                        )
                        .when_some(self.tests.get(&m.id), |d, t| {
                            d.child(
                                div()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("· {t}")),
                            )
                        }),
                )
                .child(
                    button(format!("m-test-{}", m.id), "Test")
                        .on_click(cx.listener(move |this, _, _, cx| this.test(test.clone(), cx))),
                )
                .child(button(format!("m-rm-{}", m.id), "Remove").on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(MachinesEvent::Remove(remove.clone()))),
                ))
        });
        let typed = self.ssh_host.read(cx).value().to_lowercase();
        let suggestions: Vec<String> = self
            .hosts
            .iter()
            .filter(|h| !typed.is_empty() && h.to_lowercase().contains(&typed) && **h != typed)
            .take(8)
            .cloned()
            .collect();
        let can_add = !typed.trim().is_empty();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(rows)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.ssh_host)))
                    .child(div().w(px(180.)).child(Input::new(&self.name)))
                    .child(
                        button("m-add".into(), "Add machine")
                            .when(!can_add, |d| d.opacity(0.5))
                            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    ),
            )
            .when(!suggestions.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .children(suggestions.into_iter().map(|h| {
                            let pick = h.clone();
                            button(format!("m-host-{h}"), "")
                                .child(h)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let pick = pick.clone();
                                    this.ssh_host
                                        .update(cx, |i, cx| i.set_value(pick, window, cx));
                                }))
                        })),
                )
            })
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_xs().text_color(theme.danger).child(e))
            })
    }
}
