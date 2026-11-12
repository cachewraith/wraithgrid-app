//! The community theme gallery (`CommunityThemes.tsx`, #24), under Settings → Appearance →
//! Theme: themes others published as GitHub repositories, listed in
//! cachewraith/wraithgrid-themes. The themes crate fetches and checks everything (off the UI
//! thread); this shows the list, installs, updates and removes, and installs a repository
//! given by its address.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use base64::Engine as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use wraithgrid_core::paths::{home_dir, themes_root};
use wraithgrid_themes::{
    all_themes, repo_from_url, AppTheme, CommunityTheme, CommunityThemes, CommunityThemesDeps,
    ThemeName,
};

use crate::theme_editor::Choice;

/// One for the app, as in main: the list is kept for five minutes between opens.
static COMMUNITY: LazyLock<RwLock<CommunityThemes>> = LazyLock::new(|| {
    RwLock::new(CommunityThemes::new(CommunityThemesDeps::live(
        themes_root(&home_dir()),
        crate::runtime::APP_VERSION,
    )))
});

const TEMPLATE: &str = "https://github.com/cachewraith/wraithgrid-theme-template";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Name,
    Author,
}

pub enum ThemesGalleryEvent {
    /// Use this installed theme.
    Use(Box<AppTheme>),
    /// Theme files changed (a theme was removed): draw the chosen one again.
    Changed,
    Close,
}

pub struct ThemesGallery {
    list: Option<Vec<CommunityTheme>>,
    error: Option<String>,
    busy: Option<String>,
    sort: Sort,
    mode: Option<ThemeName>,
    search: Entity<InputState>,
    url: Entity<InputState>,
    sort_select: Entity<SelectState<Vec<Choice>>>,
    /// Each repository's screenshot once fetched; None when it has none.
    shots: HashMap<String, Option<Arc<Image>>>,
    _subscriptions: Vec<Subscription>,
}

/// The installed theme with this id, read from the themes folder.
fn installed(id: &str) -> Option<AppTheme> {
    all_themes(&themes_root(&home_dir()))
        .0
        .into_iter()
        .find(|t| t.id == id)
}

fn shot(repo: &str) -> Option<Arc<Image>> {
    let url = COMMUNITY.read().ok()?.screenshot(repo)?;
    let b64 = url.strip_prefix("data:image/png;base64,")?;
    let png = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    Some(Arc::new(Image::from_bytes(ImageFormat::Png, png)))
}

impl ThemesGallery {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search themes and authors…"));
        let url = cx.new(|cx| {
            InputState::new(window, cx).placeholder("https://github.com/owner/theme-repo")
        });
        let sorts = vec![Choice::new("name", "Name"), Choice::new("author", "Author")];
        let sort_select =
            cx.new(|cx| SelectState::new(sorts, Some(IndexPath::default().row(0)), window, cx));
        let subs = vec![
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &url,
                window,
                |this, input, e: &InputEvent, window, cx| match e {
                    InputEvent::PressEnter { .. } => this.install_url(input.clone(), window, cx),
                    _ => cx.notify(),
                },
            ),
            cx.subscribe(&sort_select, |this, _, e: &SelectEvent<Vec<Choice>>, cx| {
                let SelectEvent::Confirm(Some(v)) = e else {
                    return;
                };
                this.sort = if v.as_ref() == "author" {
                    Sort::Author
                } else {
                    Sort::Name
                };
                cx.notify();
            }),
        ];
        let mut gallery = ThemesGallery {
            list: None,
            error: None,
            busy: None,
            sort: Sort::Name,
            mode: None,
            search,
            url,
            sort_select,
            shots: HashMap::new(),
            _subscriptions: subs,
        };
        gallery.reload(cx);
        gallery
    }

    /// The list again (installed state from the lock), then any screenshot not yet fetched.
    fn reload(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let mut c = COMMUNITY.write().map_err(|e| e.to_string())?;
                    c.list().map_err(|e| e.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(l) => this.list = Some(l.themes),
                    Err(e) => {
                        this.list = Some(Vec::new());
                        this.error = Some(e);
                    }
                }
                this.fetch_shots(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn fetch_shots(&mut self, cx: &mut Context<Self>) {
        let repos: Vec<String> = self
            .list
            .iter()
            .flatten()
            .map(|t| t.repo.clone())
            .filter(|r| !self.shots.contains_key(r))
            .collect();
        for repo in repos {
            cx.spawn(async move |this, cx| {
                let r = repo.clone();
                let img = cx.background_spawn(async move { shot(&r) }).await;
                let _ = this.update(cx, |this, cx| {
                    this.shots.insert(repo, img);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Installs (or updates) a repository, given as `owner/repo` or its address; then uses it.
    fn install(&mut self, repo: String, cx: &mut Context<Self>) -> Task<()> {
        self.busy = Some(repo.clone());
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let Some(repo) = repo_from_url(&repo) else {
                        return Err(
                            "Give a GitHub repository: owner/repo or its github.com address."
                                .to_string(),
                        );
                    };
                    let r = COMMUNITY.read().map_err(|e| e.to_string())?.install(&repo);
                    match r {
                        Ok(Ok(id)) => {
                            eprintln!("[wraithgrid] installed community theme {id} from {repo}");
                            Ok(installed(&id))
                        }
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err("Could not install the theme".to_string()),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match r {
                    Err(e) => this.error = Some(e),
                    Ok(t) => {
                        if let Some(t) = t {
                            cx.emit(ThemesGalleryEvent::Use(Box::new(t)));
                        }
                        this.reload(cx);
                    }
                }
                cx.notify();
            });
        })
    }

    fn install_url(
        &mut self,
        input: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let url = input.read(cx).value().trim().to_string();
        if url.is_empty() || self.busy.is_some() {
            return;
        }
        let task = self.install(url, cx);
        cx.spawn_in(window, async move |_, cx| {
            task.await;
            let _ = input.update_in(cx, |i, window, cx| i.set_value("", window, cx));
        })
        .detach();
    }

    fn remove(&mut self, id: String, cx: &mut Context<Self>) {
        let r = match COMMUNITY.read() {
            Ok(c) => match c.uninstall(&id) {
                Ok(true) => Ok(()),
                Ok(false) => Err("Not a community theme.".to_string()),
                Err(_) => Err("Could not remove the theme".to_string()),
            },
            Err(e) => Err(e.to_string()),
        };
        match r {
            Ok(()) => {
                cx.emit(ThemesGalleryEvent::Changed);
                self.reload(cx);
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    fn shown(&self, cx: &App) -> Vec<CommunityTheme> {
        let needle = self.search.read(cx).value().trim().to_lowercase();
        let mut out: Vec<CommunityTheme> = self
            .list
            .iter()
            .flatten()
            .filter(|t| self.mode.is_none_or(|m| t.modes.contains(&m)))
            .filter(|t| {
                needle.is_empty()
                    || format!("{} {} {} {}", t.name, t.author, t.description, t.repo)
                        .to_lowercase()
                        .contains(&needle)
            })
            .cloned()
            .collect();
        let key = |t: &CommunityTheme| match self.sort {
            Sort::Name => t.name.to_lowercase(),
            Sort::Author => t.author.to_lowercase(),
        };
        out.sort_by_key(key);
        out
    }

    fn card(&self, t: &CommunityTheme, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let small = |id: String, text: String, primary: bool| {
            div()
                .id(SharedString::from(id))
                .px_2()
                .py(px(2.))
                .rounded_md()
                .border_1()
                .text_xs()
                .cursor_pointer()
                .border_color(if primary { theme.primary } else { theme.border })
                .when(primary, |d| {
                    d.bg(theme.primary).text_color(theme.primary_foreground)
                })
                .when(!primary, |d| d.hover(|d| d.bg(theme.secondary)))
                .child(text)
        };
        let shot: AnyElement = match self.shots.get(&t.repo) {
            None => div().size_full().bg(theme.muted).into_any_element(),
            Some(None) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.muted)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("No screenshot")
                .into_any_element(),
            Some(Some(img_src)) => img(img_src.clone())
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
        };
        let busy = self.busy.as_deref() == Some(t.repo.as_str());
        let repo = t.repo.clone();
        let action: AnyElement = if t.too_new {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "Needs Wraithgrid {}",
                    t.min_app_version.clone().unwrap_or_default()
                ))
                .into_any_element()
        } else if t.update || t.installed_id.is_none() {
            let text = if t.update {
                "Update"
            } else if busy {
                "Installing…"
            } else {
                "Install"
            };
            small(format!("ctg-install-{}", t.repo), text.into(), t.update)
                .when(busy, |d| d.opacity(0.5))
                .when(!busy, |d| {
                    d.on_click(
                        cx.listener(move |this, _, _, cx| this.install(repo.clone(), cx).detach()),
                    )
                })
                .into_any_element()
        } else {
            let id = t.installed_id.clone().unwrap_or_default();
            small(format!("ctg-use-{}", t.repo), "Use".into(), false)
                .on_click(cx.listener(move |_, _, _, cx| {
                    if let Some(t) = installed(&id) {
                        cx.emit(ThemesGalleryEvent::Use(Box::new(t)));
                    }
                }))
                .into_any_element()
        };
        let badge = |text: String, up: bool| {
            let c = if up { theme.link } else { theme.success };
            div()
                .px(px(7.))
                .rounded_full()
                .bg(if up { theme.ring } else { theme.success }.opacity(0.15))
                .text_color(c)
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child(text)
        };
        let page = format!("https://github.com/{}", t.repo);
        div()
            .id(SharedString::from(format!("ctg-{}", t.repo)))
            .flex()
            .flex_col()
            .w(px(228.))
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .overflow_hidden()
            .hover(|d| d.border_color(theme.input))
            .child(div().w_full().h(px(140.)).child(shot))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .px_3()
                    .py(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(t.name.clone())
                            .when(t.update, |d| {
                                d.child(badge(format!("Update {}", t.version), true))
                            })
                            .when(!t.update && t.installed_id.is_some(), |d| {
                                d.child(badge("Installed".into(), false))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("by {}", t.author)),
                    )
                    .when(!t.description.is_empty(), |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(t.description.clone()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .mt(px(2.))
                            .children(t.modes.iter().map(|m| {
                                div()
                                    .px(px(6.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(theme.input)
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(m.as_str())
                            }))
                            .child(
                                div()
                                    .ml_auto()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!(
                                        "v{}",
                                        t.installed_version.as_ref().unwrap_or(&t.version)
                                    )),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .py_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        small(format!("ctg-repo-{}", t.repo), "Repository".into(), false)
                            .on_click(move |_, _, cx| cx.open_url(&page)),
                    )
                    .child(div().flex_1())
                    .when_some(t.installed_id.clone(), |d, id| {
                        d.child(
                            small(format!("ctg-remove-{}", t.repo), "Remove".into(), false)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.remove(id.clone(), cx)),
                                ),
                        )
                    })
                    .child(action),
            )
    }
}

impl EventEmitter<ThemesGalleryEvent> for ThemesGallery {}

impl Render for ThemesGallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let hint = |text: String| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let modes = [
            (None, "All"),
            (Some(ThemeName::Dark), "Dark"),
            (Some(ThemeName::Light), "Light"),
        ];
        let seg = div()
            .flex()
            .gap_1()
            .children(modes.into_iter().map(|(m, label)| {
                let on = self.mode == m;
                div()
                    .id(SharedString::from(format!("ctg-mode-{label}")))
                    .px(px(10.))
                    .py(px(3.))
                    .rounded_full()
                    .border_1()
                    .text_sm()
                    .cursor_pointer()
                    .border_color(if on { theme.ring } else { theme.input })
                    .text_color(if on {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .when(on, |d| d.bg(theme.ring.opacity(0.12)))
                    .hover(|d| d.bg(theme.secondary))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mode = m;
                        cx.notify();
                    }))
            }));
        let button = |id: &'static str, text: &'static str, enabled: bool| {
            div()
                .id(id)
                .flex_none()
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .text_sm()
                .when(enabled, |d| {
                    d.cursor_pointer().hover(|d| d.bg(theme.secondary))
                })
                .when(!enabled, |d| d.opacity(0.5))
                .child(text)
        };

        let tools = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w(px(180.))
                    .child(Input::new(&self.search).small()),
            )
            .child(
                div()
                    .w(px(110.))
                    .child(Select::new(&self.sort_select).small()),
            )
            .child(seg)
            .child(
                button("ctg-close", "Close", true)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ThemesGalleryEvent::Close))),
            );

        let shown = self.shown(cx);
        let body: AnyElement = match &self.list {
            None => hint("Loading the community list…".into()).into_any_element(),
            Some(list) if shown.is_empty() && self.error.is_none() => hint(if list.is_empty() {
                "No community themes yet.".into()
            } else {
                "No theme matches.".into()
            })
            .into_any_element(),
            Some(_) => div()
                .flex()
                .flex_wrap()
                .gap_3()
                .children(shown.iter().map(|t| self.card(t, cx)))
                .into_any_element(),
        };

        let url_set = !self.url.read(cx).value().trim().is_empty();
        let can_install = url_set && self.busy.is_none();
        let url = self.url.clone();
        let form = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w(px(180.))
                    .child(Input::new(&self.url).small()),
            )
            .child(
                button("ctg-url-install", "Install from GitHub", can_install).when(
                    can_install,
                    |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            this.install_url(url.clone(), window, cx)
                        }))
                    },
                ),
            );

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p(px(14.))
            .rounded_xl()
            .border_1()
            .border_color(theme.input)
            .bg(theme.popover)
            .child(tools)
            .when_some(self.error.clone(), |d, e| {
                d.child(
                    div()
                        .px(px(10.))
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.danger.opacity(0.45))
                        .bg(theme.danger.opacity(0.08))
                        .text_sm()
                        .child(e),
                )
            })
            .child(body)
            .child(form)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Themes are data only — colors, fonts and shapes — checked before they install. Make your own from the\u{a0}")
                    .child(
                        div()
                            .id("ctg-template")
                            .cursor_pointer()
                            .text_color(theme.link)
                            .hover(|d| d.underline())
                            .child("theme template")
                            .on_click(|_, _, cx| cx.open_url(TEMPLATE)),
                    )
                    .child("."),
            )
    }
}
