//! A chat answer streamed into a markdown bubble (gpui-component's TextView, parsed
//! incrementally off the main thread) with a code block, a table and a diff tool card.
//! The chunks are canned; in the app they come from a chat adapter's events.

use std::time::Duration;

use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

const ANSWER: &str = r#"I found the cause. **`resolveShellEnv`** runs the login shell once, but fish
needs `-l -c` split, so `$PATH` came back empty and every pane fell back to `/usr/bin`.

### What changed

1. Ask the shell with flags per kind (`sh`, `fish`, `csh`).
2. Fall back to `/bin/sh -l` when the first try fails.
3. Cache the answer per launch.

```rust
fn login_flags(shell: ShellKind) -> &'static [&'static str] {
    match shell {
        ShellKind::Csh => &["-c"],
        ShellKind::Fish | ShellKind::Posix => &["-l", "-c"],
    }
}
```

| Shell | Flags      | Before | After |
|-------|------------|--------|-------|
| bash  | `-l -c`    | ok     | ok    |
| fish  | `-l -c`    | empty  | ok    |
| tcsh  | `-c`       | error  | ok    |

The diff is below. Run `bun run test` before committing.
"#;

const PATCH: &str = r#"diff --git a/src/main/shell-env.ts b/src/main/shell-env.ts
--- a/src/main/shell-env.ts
+++ b/src/main/shell-env.ts
@@ -12,6 +12,11 @@ export function loginFlags(shell: string): string[] {
   const name = basename(shell)
-  if (name === 'csh' || name === 'tcsh') return ['-c']
-  return ['-lc']
+  switch (shellKind(name)) {
+    case 'csh':
+      return ['-c']
+    case 'fish':
+    case 'posix':
+      return ['-l', '-c']
+  }
 }

 export async function resolveShellEnv(shell: string) {
"#;

pub struct ChatView {
    answer: Entity<TextViewState>,
    diff: Entity<DiffState>,
    streaming: bool,
    _stream: Option<Task<()>>,
}

impl ChatView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let answer = cx.new(|cx| TextViewState::markdown("", cx));
        let files = DiffFile::parse(PATCH).unwrap_or_else(|err| {
            eprintln!("[chat] diff parse failed: {err}");
            Vec::new()
        });
        let diff = cx.new(|cx| DiffState::new(files, cx));
        let mut this = Self {
            answer,
            diff,
            streaming: false,
            _stream: None,
        };
        this.restart(cx);
        this
    }

    /// Streams the answer as claude does: a few words every ~60 ms.
    fn restart(&mut self, cx: &mut Context<Self>) {
        self.answer.update(cx, |state, cx| state.set_text("", cx));
        self.streaming = true;
        let answer = self.answer.clone();
        self._stream = Some(cx.spawn(async move |this, cx| {
            let mut rest = ANSWER;
            while !rest.is_empty() {
                cx.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                let mut end = 0;
                for (words, (i, _)) in rest.match_indices(' ').enumerate() {
                    end = i + 1;
                    if words == 5 {
                        break;
                    }
                }
                if end == 0 || end >= rest.len() {
                    end = rest.len();
                }
                let (chunk, tail) = rest.split_at(end);
                rest = tail;
                answer.update(cx, |state, cx| state.push_str(chunk, cx));
            }
            let _ = this.update(cx, |this, cx| {
                this.streaming = false;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bubble = |id: &'static str| {
            div()
                .id(id)
                .flex_shrink_0()
                .rounded_lg()
                .p_3()
                .max_w(px(760.))
        };
        div()
            .id("chat")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.background)
            .text_color(theme.foreground)
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div().flex().justify_end().child(
                    bubble("user")
                        .bg(theme.secondary)
                        .child("Panes started from the launcher can't find claude on fish. Why?"),
                ),
            )
            .child(
                bubble("answer")
                    .bg(theme.muted)
                    .child(TextView::new(&self.answer).selectable(true)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(760.))
                    .h(px(300.))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(Diff::new(&self.diff)),
            )
            .child(
                div()
                    .id("replay")
                    .w(px(160.))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .child(if self.streaming {
                        "Streaming…"
                    } else {
                        "Replay stream"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.restart(cx))),
            )
    }
}
