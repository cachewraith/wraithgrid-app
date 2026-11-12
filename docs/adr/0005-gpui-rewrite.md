# ADR 0005: Rewrite the desktop app in Rust with GPUI

- Status: **Proposed** (2026-11-12), issue #166. Phase 0 (spike) results for Linux below;
  Windows, macOS and the hands-on input checks are still open, so this is not yet a go.

## Context

#166 decides a full rewrite from Electron + React to Rust with GPUI (smaller install, less
memory, faster start, one language, a harder-to-lift binary). Phase 0 is a two-week spike that
must prove the risky parts on every OS before any port starts: a terminal grid at `yes` speed,
terminal text input, a streaming markdown chat bubble, a webview pane that captures console
errors, tiling-compositor behaviour, and size/RAM next to Electron. Exit: all pass, or each gap
has a known workaround.

The spike is `gpui-spike/` (one binary, `wraithgrid-spike`): a 2×2 terminal grid, a Chat tab
and a Preview tab in one window with Wraithgrid's identity. `--bench N` prints frame gaps,
throughput and memory, then quits. CI (`.github/workflows/gpui.yml`) builds and tests it
on Linux, Windows and macOS (arm64 + x64) and keeps the release binaries for hand testing.

## Decision (proposed)

1. **Pin `gpui-kit` / `gpui-component` 0.9.1**, which pins `gpui-pre` =0.3.12 (a crates.io
   snapshot of `zed@d2a4fb6`), with `gpui-webview` 0.9.1 (`lb-wry` 0.53.3). All Apache-2.0.
   One version bump moves GPUI and the components together; no git dependency on Zed. Zed's
   GPL crates (terminal_view, editor, markdown) are not used or copied.
2. **Terminal:** `alacritty_terminal` 0.26 for the grid and parser, our own GPUI element for
   drawing (cells batched by style, shaped with a forced cell width, wide glyphs on their own),
   GPUI's `EntityInputHandler` for IME. A reader thread parses in 4 KB slices under the term
   lock and wakes the UI at most once per frame. In the app the bytes come from the pane host
   socket (ADR 0004) instead of an in-process pty; the shape is the same.
3. **Chat:** gpui-component's `TextView` (markdown parsed incrementally off the main thread,
   `push_str` per chunk, built-in stream fade) and its `Diff` view for tool-card diffs.
4. **Preview:** `gpui-webview` (Wry). Console errors, uncaught exceptions, rejections and failed
   fetch/XHR/resources are caught by an initialization script and posted over Wry's IPC.
5. **Window chrome from the env, as `platform.ts` does:** Hyprland grants server-side
   decorations and draws none, so GPUI's `Decorations` alone can't tell us to show a close
   button. `chrome.rs` ports `detectDesktop`; app id `wraithgrid`, minimum 640×420.

## Spike results (Linux, Hyprland 0.57.2, NVIDIA, 144 Hz, 2026-11-12)

| Check | Result |
|---|---|
| 2×2 grid, `yes` in every pane (release, native Wayland) | 120–143 fps (display rate), frame gap p50 7 ms / p99 9.6–14.8 ms / max 28–33 ms, ~6 MB/s parsed per pane |
| Same before slicing the parse | 28 fps, p95 41 ms: the UI waited on 64 KB parses for the lock (fixed, see 2) |
| Same on Xvfb (X11, software GL) | 21 fps, steady 48 ms gaps, ~5.5 MB/s per pane |
| Wayland app id | `class: wraithgrid`, `xwayland: false`, tiled: existing window rules keep working |
| Title bar on Hyprland | close only (Server decorations + `HYPRLAND_INSTANCE_SIGNATURE`) |
| Streaming markdown bubble | headings, lists, inline code, fenced code, table; streams at ~6 words/60 ms |
| Diff tool card | file header, hunk header, line numbers, word-level changes |
| Webview + console capture (X11) | console.error with message + stack, uncaught `ReferenceError` with file:line, unhandled rejection, failed fetch, failed image |
| Release binary | 31.7 MB stripped (LTO); **8.6 MB xz**, 12.7 MB gzip. Electron 1.26.0 installers: 90–136 MB |
| Memory, 4 idle panes | 123 MB PSS, 49 MB private (RSS 357 MB is mostly shared NVIDIA/Mesa libraries) |
| Memory, 4 panes after `yes` (scrollback full) | 202 MB PSS, 128 MB private |
| Electron, for comparison | ~713 MB PSS across its 8 processes (a real session with 3 chats: not the same workload) |

## Gaps and workarounds

- **Webview on native Wayland: not possible.** WebKitGTK can only embed into an X11 parent, so
  gpui-webview returns an error on a Wayland window. Workarounds, in order of preference:
  run the whole app under XWayland on Linux (`--x11`; loses native Wayland input/scaling), or
  give the preview its own top-level window on Wayland (a GTK window with the same app id, which
  tiles next to the main one). To decide in phase 4; terminals and chat don't need it.
- **`GDK_BACKEND=wayland` in the session breaks the webview** (GDK ignores
  `set_allowed_backends("x11")`). The app forces `GDK_BACKEND=x11` for itself and gives panes
  the user's value back (`USER_GDK_BACKEND` in `term.rs`).
- **"Script error."** for exceptions in inline HTML (`about:blank`); a real dev-server URL gives
  the full message and location, and preview only ever loads URLs.
- **No request interception** in a system webview (Electron's session did it): failed requests
  are caught in the page (fetch/XHR wrappers, resource `error` events). Requests made before the
  init script runs, and service-worker fetches, are missed.
- **Fonts:** GPUI's `monospace` does not resolve to a mono face on Linux. The spike picks the
  first installed of a list; the app should bundle one mono font (OFL) so the grid is the same
  everywhere.
- **Scrollback memory:** an alacritty cell is 24 bytes, so 10 000 lines × ~230 columns is ~55 MB
  per full pane. Keep the Electron app's scrollback length or cap by bytes.
- **Code-block highlighting** needs gpui-component's `tree-sitter-*` features (off in the spike).
- **wgpu loads every Vulkan driver** (Mesa's libLLVM next to NVIDIA's): shared memory, but
  startup cost; check `WGPU_BACKEND` / ICD filtering in phase 6.

## Not verified yet (blocks the go)

- **Windows and macOS** (arm64 + x64): only built and unit-tested in CI so far; run the CI
  artifacts by hand: grid with `--cmd yes --bench 15`, IME, paste, preview.
- **By hand on Linux:** IME (fcitx5/ibus) composition and candidate window position, paste of
  a Wayland screenshot (Ctrl+V in a pane pastes a saved file path), mouse selection + Ctrl+Shift+C,
  Shift+PageUp scrollback. The code paths exist; no automation here can type into a Wayland
  window safely.
- **X11 (a real X server)** for the title bar and app id (`WM_CLASS`).

## Consequences

- If the open checks pass, phase 1 starts as a Cargo workspace (`core`, `host` from `host-rs`,
  `app`, `cli`) and `gpui-spike/` is deleted; until then it is throwaway code, not a base.
- Linux packages gain a WebKitGTK 4.1 dependency (deb/rpm/pacman declare it; the AppImage must
  bundle or require it).
- gpui-component's releases set the GPUI version: an upgrade means absorbing GPUI's breaking
  changes at their pace, not Zed's.
