//! Sign in without a terminal (`LoginDialog.tsx` + `Logins` in login.ts): the CLI's own login
//! runs over pipes in the account's folder; this shows its link, takes the code the page may
//! show (claude) or shows the one-time code to type there (codex), and says who is signed in
//! at the end. Tokens never pass through Wraithgrid.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::a11y::A11y as _;
use wraithgrid_core::login::{last_line, login_url, AuthStatus, LoginFlow};

const OUTPUT_MAX: usize = 64_000;

/// The running login and its stdin (claude reads the pasted code there).
type Running = Arc<Mutex<Option<(Child, Option<ChildStdin>)>>>;

pub enum LoginEvent {
    SignedIn,
    UseTerminal,
    Close,
}

enum Step {
    Starting,
    Url {
        url: String,
        sent: bool,
        device_code: Option<String>,
    },
    Done(AuthStatus),
    Error(String),
}

/// What the login process says, from its reader threads.
enum Out {
    Chunk(String),
    Exit(Option<String>),
}

pub struct LoginDialog {
    flow: LoginFlow,
    program: PathBuf,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    name: String,
    service: &'static str,
    step: Step,
    out: String,
    code: Entity<InputState>,
    child: Running,
    _rx: Option<Task<()>>,
}

impl LoginDialog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        flow: LoginFlow,
        program: PathBuf,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
        name: String,
        codex: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("Code"));
        cx.subscribe(&code, |this, _, e: &InputEvent, cx| match e {
            InputEvent::PressEnter { .. } => this.submit(cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        })
        .detach();
        let mut dialog = LoginDialog {
            flow,
            program,
            env,
            cwd,
            name,
            service: if codex { "ChatGPT (Codex)" } else { "Claude" },
            step: Step::Starting,
            out: String::new(),
            code,
            child: Arc::new(Mutex::new(None)),
            _rx: None,
        };
        dialog.start(cx);
        dialog
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.env);
        cmd
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.cancel();
        self.step = Step::Starting;
        self.out.clear();
        let mut cmd = self.command(self.flow.login);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Out>();
        match cmd.spawn() {
            Ok(mut child) => {
                for pipe in [
                    child
                        .stdout
                        .take()
                        .map(|p| Box::new(p) as Box<dyn Read + Send>),
                    child
                        .stderr
                        .take()
                        .map(|p| Box::new(p) as Box<dyn Read + Send>),
                ]
                .into_iter()
                .flatten()
                {
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        let mut pipe = pipe;
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = pipe.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                            if tx.unbounded_send(Out::Chunk(chunk)).is_err() {
                                break;
                            }
                        }
                    });
                }
                let stdin = child.stdin.take();
                *self.child.lock().unwrap() = Some((child, stdin));
                let (waiter, tx) = (self.child.clone(), tx);
                std::thread::spawn(move || loop {
                    let done = {
                        let mut g = waiter.lock().unwrap();
                        match g.as_mut().map(|(c, _)| c.try_wait()) {
                            Some(Ok(Some(_))) | Some(Err(_)) | None => true,
                            Some(Ok(None)) => false,
                        }
                    };
                    if done {
                        let _ = tx.unbounded_send(Out::Exit(None));
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                });
            }
            Err(e) => {
                let _ = tx.unbounded_send(Out::Exit(Some(format!(
                    "Could not run {}: {e}",
                    self.program.display()
                ))));
            }
        }
        self._rx = Some(cx.spawn(async move |this, cx| {
            while let Some(o) = rx.next().await {
                let stop = matches!(o, Out::Exit(_));
                if this.update(cx, |this, cx| this.output(o, cx)).is_err() || stop {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn output(&mut self, o: Out, cx: &mut Context<Self>) {
        match o {
            Out::Chunk(text) => {
                if self.out.len() < OUTPUT_MAX {
                    self.out.push_str(&text);
                }
                if matches!(self.step, Step::Starting) {
                    let url = login_url(&self.out, self.flow.hosts);
                    // A device sign-in shows its code after the link: wait for both.
                    let code = self.flow.device_code(&self.out);
                    if let (Some(url), true) = (url, code.is_some() || !self.flow.device) {
                        self.step = Step::Url {
                            url,
                            sent: false,
                            device_code: code,
                        };
                    }
                }
            }
            Out::Exit(error) => self.finish(error, cx),
        }
        cx.notify();
    }

    /// The login ended: the status command says whether it worked.
    fn finish(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        *self.child.lock().unwrap() = None;
        let mut cmd = self.command(self.flow.status);
        let (stderr, last) = (self.flow.status_stderr, last_line(&self.out));
        cx.spawn(async move |this, cx| {
            let text = cx
                .background_spawn(async move {
                    cmd.output()
                        .map(|o| {
                            let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
                            if stderr {
                                t.push_str(&String::from_utf8_lossy(&o.stderr));
                            }
                            t
                        })
                        .unwrap_or_default()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let s = this.flow.parse_status(&text);
                if s.logged_in {
                    this.step = Step::Done(s);
                    cx.emit(LoginEvent::SignedIn);
                } else {
                    this.step = Step::Error(error.unwrap_or_else(|| {
                        if last.is_empty() {
                            "Sign-in did not finish.".into()
                        } else {
                            last
                        }
                    }));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The code the sign-in page showed, typed into claude's prompt.
    fn submit(&mut self, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().trim().to_string();
        let ok = !code.is_empty()
            && code.len() <= 2048
            && code.bytes().all(|b| (0x21..=0x7e).contains(&b));
        let Step::Url {
            sent,
            device_code: None,
            ..
        } = &mut self.step
        else {
            return;
        };
        if !ok || *sent {
            return;
        }
        if let Some((_, Some(stdin))) = self.child.lock().unwrap().as_mut() {
            let _ = stdin.write_all(format!("{code}\n").as_bytes());
            let _ = stdin.flush();
        }
        *sent = true;
        cx.notify();
    }

    fn cancel(&mut self) {
        if let Some((mut child, _)) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for LoginDialog {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl EventEmitter<LoginEvent> for LoginDialog {}

impl Render for LoginDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let hint = |t: String| div().text_sm().text_color(theme.muted_foreground).child(t);
        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .a11y_button(label)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(primary, |d| {
                    d.bg(theme.primary).text_color(theme.primary_foreground)
                })
                .when(!primary, |d| {
                    d.border_1()
                        .border_color(theme.border)
                        .hover(|d| d.bg(theme.secondary))
                })
                .child(label)
        };
        let title = match &self.step {
            Step::Done(_) => format!("{} is signed in", self.name),
            _ => format!("Sign in to {} as {}", self.service, self.name),
        };
        let mut body: Vec<AnyElement> = vec![];
        let mut actions: Vec<AnyElement> = vec![];
        match &self.step {
            Step::Starting => {
                body.push(hint("Starting the sign-in…".into()).into_any_element());
                actions.push(
                    button("login-cancel", "Cancel", false)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(LoginEvent::Close)))
                        .into_any_element(),
                );
            }
            Step::Url {
                url,
                sent,
                device_code,
            } => {
                let (open, copy) = (url.clone(), url.clone());
                let link = div()
                    .flex()
                    .gap_2()
                    .child(
                        button("login-open", "Open sign-in page", true)
                            .on_click(move |_, _, cx| cx.open_url(&open)),
                    )
                    .child(
                        button("login-copy", "Copy link", false).on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        }),
                    );
                if let Some(code) = device_code {
                    let copy_code = code.clone();
                    body.push(
                        hint("Open the sign-in page and enter this one-time code there.".into())
                            .into_any_element(),
                    );
                    body.push(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .bg(theme.secondary)
                                    .font_family(crate::terminal::mono(cx))
                                    .text_lg()
                                    .child(code.clone()),
                            )
                            .child(button("login-copy-code", "Copy code", false).on_click(
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copy_code.clone(),
                                    ))
                                },
                            ))
                            .into_any_element(),
                    );
                    body.push(link.into_any_element());
                    body.push(
                        hint("This closes by itself once you finish in the browser.".into())
                            .into_any_element(),
                    );
                } else {
                    body.push(
                        hint(
                            "Your browser opens Claude’s sign-in page. If it didn’t, open it here."
                                .into(),
                        )
                        .into_any_element(),
                    );
                    body.push(link.into_any_element());
                    body.push(
                        hint("If the page shows a code, paste it here.".into()).into_any_element(),
                    );
                    body.push(Input::new(&self.code).into_any_element());
                    if *sent {
                        body.push(hint("Checking the code…".into()).into_any_element());
                    }
                }
                actions.push(
                    button("login-cancel", "Cancel", false)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(LoginEvent::Close)))
                        .into_any_element(),
                );
                if device_code.is_none() {
                    let can = !*sent && !self.code.read(cx).value().trim().is_empty();
                    actions.push(
                        button("login-continue", "Continue", true)
                            .when(!can, |d| d.opacity(0.5))
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx)))
                            .into_any_element(),
                    );
                }
            }
            Step::Done(s) => {
                let who = [
                    Some(
                        s.email
                            .as_ref()
                            .map_or("Signed in".to_string(), |e| format!("Signed in as {e}")),
                    ),
                    s.plan.clone(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                body.push(hint(format!("{who}.")).into_any_element());
                actions.push(
                    button("login-done", "Done", true)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(LoginEvent::Close)))
                        .into_any_element(),
                );
            }
            Step::Error(e) => {
                body.push(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(e.clone())
                        .into_any_element(),
                );
                actions.push(
                    button("login-terminal", "Use a terminal instead", false)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(LoginEvent::UseTerminal)))
                        .into_any_element(),
                );
                actions.push(
                    button("login-retry", "Try again", true)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.code.update(cx, |i, cx| i.set_value("", window, cx));
                            this.start(cx);
                        }))
                        .into_any_element(),
                );
            }
        }
        div()
            .id("login-dialog")
            .a11y_dialog(title.clone())
            .w(px(440.))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.emit(LoginEvent::Close);
                }
            }))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .children(body)
            .child(div().flex().justify_end().gap_2().children(actions))
    }
}
