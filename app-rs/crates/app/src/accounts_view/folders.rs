//! Account folders on the Accounts page (`SidebarAccounts.tsx`): one level of folders; drag an
//! account onto a folder (or onto the top level) to move it; double-click or right-click a
//! folder to rename it. The view says what to change; the window changes the config.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::accounts::FOLDERS_MAX;
use wraithgrid_core::config::{AccountFolder, Config};

use super::{AccountsEvent, AccountsView};
use crate::a11y::A11y as _;

#[derive(Clone, Debug, PartialEq)]
pub enum FolderOp {
    Create(String),
    Rename(String, String),
    Toggle(String),
    Delete(String),
    /// An account into a folder, or out of any (None).
    Move(String, Option<String>),
}

/// What the folders section shows: the folders and each account's place.
pub(super) struct Folders {
    folders: Vec<AccountFolder>,
    /// (id, name, signed in, folder).
    accounts: Vec<(String, String, bool, Option<String>)>,
    renaming: Option<(String, Entity<InputState>, Subscription)>,
    menu: Option<(String, Point<Pixels>)>,
}

impl Folders {
    pub(super) fn new(config: &Config) -> Self {
        let mut f = Folders {
            folders: vec![],
            accounts: vec![],
            renaming: None,
            menu: None,
        };
        f.set(config);
        f
    }

    pub(super) fn set(&mut self, config: &Config) {
        self.folders = config.account_folders.clone();
        self.accounts = config
            .accounts
            .iter()
            .map(|a| {
                (
                    a.id.clone(),
                    a.name.clone(),
                    a.signed_in,
                    a.folder_id.clone(),
                )
            })
            .collect();
        if let Some((id, ..)) = &self.renaming {
            if !self.folders.iter().any(|f| &f.id == id) {
                self.renaming = None;
            }
        }
    }
}

/// An account being dragged; drawn under the pointer.
#[derive(Clone)]
struct AccountDrag {
    id: String,
    name: String,
}

impl Render for AccountDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.ring)
            .bg(theme.popover)
            .shadow_lg()
            .text_sm()
            .child(self.name.clone())
    }
}

impl AccountsView {
    pub fn set_folders(&mut self, config: &Config, cx: &mut Context<Self>) {
        self.folders.set(config);
        cx.notify();
    }

    fn folder_op(&mut self, op: FolderOp, cx: &mut Context<Self>) {
        self.folders.menu = None;
        cx.emit(AccountsEvent::Folder(op));
    }

    fn new_folder(&mut self, cx: &mut Context<Self>) {
        let n = self.folders.folders.len() + 1;
        self.folder_op(FolderOp::Create(format!("Folder {n}")), cx);
    }

    fn start_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.folders.folders.iter().find(|f| f.id == id) else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(f.name.clone()));
        let fid = id.to_string();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, e: &InputEvent, _, cx| match e {
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    let name = input.read(cx).value().to_string();
                    this.folders.renaming = None;
                    this.folder_op(FolderOp::Rename(fid.clone(), name), cx);
                }
                _ => {}
            },
        );
        window.focus(&input.focus_handle(cx), cx);
        self.folders.renaming = Some((id.to_string(), input, sub));
        self.folders.menu = None;
        cx.notify();
    }

    fn account_row(&self, id: &str, name: &str, signed_in: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let drag = AccountDrag {
            id: id.to_string(),
            name: name.to_string(),
        };
        div()
            .id(SharedString::from(format!("acc-row-{id}")))
            .role(Role::ListItem)
            .aria_label(format!("{name} (drag into a folder)"))
            .h(px(28.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .text_sm()
            .cursor_grab()
            .hover(|d| d.bg(theme.secondary))
            .child(
                div()
                    .size(px(18.))
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        name.chars()
                            .next()
                            .map(|c| c.to_uppercase().to_string())
                            .unwrap_or_default(),
                    ),
            )
            .child(div().flex_1().truncate().child(name.to_string()))
            .when(!signed_in, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child("login needed"),
                )
            })
            .on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
            .into_any_element()
    }

    pub(super) fn folders_section(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let count = self.folders.folders.len();
        let loose: Vec<_> = self
            .folders
            .accounts
            .iter()
            .filter(|a| {
                a.3.as_ref()
                    .is_none_or(|f| !self.folders.folders.iter().any(|x| &x.id == f))
            })
            .cloned()
            .collect();
        let empty_note = |t: &'static str| {
            div()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(t)
        };
        let folders: Vec<AnyElement> = self
            .folders
            .folders
            .clone()
            .into_iter()
            .map(|f| {
                let members: Vec<_> = self
                    .folders
                    .accounts
                    .iter()
                    .filter(|a| a.3.as_deref() == Some(f.id.as_str()))
                    .cloned()
                    .collect();
                let renaming = self
                    .folders
                    .renaming
                    .as_ref()
                    .filter(|(id, ..)| *id == f.id)
                    .map(|(_, input, _)| input.clone());
                let (tg, rn, menu_id, rm, drop_to) = (
                    f.id.clone(),
                    f.id.clone(),
                    f.id.clone(),
                    f.id.clone(),
                    f.id.clone(),
                );
                let head = div()
                    .id(SharedString::from(format!("fold-hd-{}", f.id)))
                    .h(px(30.))
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_md()
                    .hover(|d| d.bg(theme.secondary))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            this.folders.menu = Some((menu_id.clone(), e.position));
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("fold-tg-{}", f.id)))
                            .role(Role::Button)
                            .aria_label(format!(
                                "{} {}",
                                if f.collapsed { "Expand" } else { "Collapse" },
                                f.name
                            ))
                            .aria_expanded(!f.collapsed)
                            .w(px(18.))
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .child(if f.collapsed { "›" } else { "⌄" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.folder_op(FolderOp::Toggle(tg.clone()), cx)
                            })),
                    )
                    .child(div().text_color(theme.muted_foreground).child("▤"))
                    .child(match renaming {
                        Some(input) => div()
                            .flex_1()
                            .child(Input::new(&input).small())
                            .into_any_element(),
                        None => div()
                            .id(SharedString::from(format!("fold-nm-{}", f.id)))
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(f.name.clone())
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(
                                    "Double-click or right-click to rename",
                                )
                                .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                                if e.click_count() == 2 {
                                    this.start_rename(&rn, window, cx);
                                }
                            }))
                            .into_any_element(),
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(members.len().to_string()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("fold-rm-{}", f.id)))
                            .a11y_button(format!("Delete folder {} (its accounts stay)", f.name))
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_color(theme.muted_foreground)
                            .hover(|d| d.bg(theme.secondary))
                            .child("×")
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(
                                    "Delete folder (its accounts stay)",
                                )
                                .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.folder_op(FolderOp::Delete(rm.clone()), cx)
                            })),
                    );
                div()
                    .id(SharedString::from(format!("fold-{}", f.id)))
                    .role(Role::Group)
                    .aria_label(format!("Folder {}", f.name))
                    .rounded_md()
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .drag_over::<AccountDrag>(|d, _, _, cx| d.border_color(cx.theme().ring))
                    .on_drop(cx.listener(move |this, drag: &AccountDrag, _, cx| {
                        this.folder_op(FolderOp::Move(drag.id.clone(), Some(drop_to.clone())), cx)
                    }))
                    .child(head)
                    .when(!f.collapsed, |d| {
                        d.child(
                            div()
                                .pl_5()
                                .flex()
                                .flex_col()
                                .when(members.is_empty(), |d| {
                                    d.child(empty_note("Drop accounts here."))
                                })
                                .children(members.iter().map(|(id, name, signed_in, _)| {
                                    self.account_row(id, name, *signed_in, cx)
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let hint = count > 0 && loose.is_empty() && !self.folders.accounts.is_empty();
        let root = div()
            .id("acc-root")
            .rounded_md()
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .min_h(px(8.))
            .drag_over::<AccountDrag>(|d, _, _, cx| d.border_color(cx.theme().ring))
            .on_drop(cx.listener(|this, drag: &AccountDrag, _, cx| {
                this.folder_op(FolderOp::Move(drag.id.clone(), None), cx)
            }))
            .children(
                loose
                    .iter()
                    .map(|(id, name, signed_in, _)| self.account_row(id, name, *signed_in, cx)),
            )
            .when(hint, |d| {
                d.child(empty_note(
                    "Drop here to take an account out of its folder.",
                ))
            });
        let menu = self.folders.menu.clone().and_then(|(id, at)| {
            let f = self.folders.folders.iter().find(|f| f.id == id)?.clone();
            Some(self.folder_menu(f, at, window, cx))
        });
        div()
            .id("acc-folders")
            .role(Role::Group)
            .aria_label("Account folders")
            .max_w(px(760.))
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .child("Folders · drag accounts to group them"),
                    )
                    .child(
                        div()
                            .id("fold-new")
                            .a11y_button("New folder")
                            .px_1()
                            .rounded_sm()
                            .when(count < FOLDERS_MAX, |d| {
                                d.cursor_pointer()
                                    .hover(|d| d.bg(theme.secondary))
                                    .on_click(cx.listener(|this, _, _, cx| this.new_folder(cx)))
                            })
                            .when(count >= FOLDERS_MAX, |d| d.opacity(0.5))
                            .child("+"),
                    ),
            )
            .when(self.folders.accounts.is_empty(), |d| {
                d.child(empty_note("No accounts yet."))
            })
            .children(folders)
            .child(root)
            .children(menu)
            .into_any_element()
    }

    fn folder_menu(
        &self,
        f: AccountFolder,
        at: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let size = window.viewport_size();
        let count = self.folders.folders.len();
        let item = |id: &'static str, label: &'static str, danger: bool, enabled: bool| {
            div()
                .id(id)
                .role(Role::MenuItem)
                .aria_label(label)
                .px_2()
                .py_1()
                .rounded_md()
                .when(enabled, |d| {
                    d.cursor_pointer().hover(|d| d.bg(theme.secondary))
                })
                .when(!enabled, |d| d.opacity(0.5))
                .when(danger, |d| d.text_color(theme.danger))
                .child(label)
        };
        let (rn, tg, rm) = (f.id.clone(), f.id.clone(), f.id.clone());
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.folders.menu = None;
            cx.notify();
        };
        let menu = div()
            .id("fold-menu")
            .role(Role::Menu)
            .aria_label(format!("Folder {}", f.name))
            .absolute()
            .left(at.x.min(size.width - px(240.)))
            .top(at.y.min(size.height - px(150.)))
            .w(px(230.))
            .p_1()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(item("fold-m-rename", "Rename", false, true).on_click(
                cx.listener(move |this, _, window, cx| this.start_rename(&rn, window, cx)),
            ))
            .child(
                item(
                    "fold-m-toggle",
                    if f.collapsed { "Expand" } else { "Collapse" },
                    false,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.folder_op(FolderOp::Toggle(tg.clone()), cx)
                })),
            )
            .child(
                item("fold-m-new", "New folder", false, count < FOLDERS_MAX)
                    .when(count < FOLDERS_MAX, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| this.new_folder(cx)))
                    }),
            )
            .child(div().h(px(1.)).my_1().bg(theme.border))
            .child(
                item("fold-m-delete", "Delete folder (accounts stay)", true, true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.folder_op(FolderOp::Delete(rm.clone()), cx)
                    }),
                ),
            );
        deferred(
            anchored().position(point(px(0.), px(0.))).child(
                div()
                    .id("fold-scrim")
                    .occlude()
                    .w(size.width)
                    .h(size.height)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| close(this, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| close(this, cx)),
                    )
                    .child(menu),
            ),
        )
        .with_priority(1)
        .into_any_element()
    }
}
