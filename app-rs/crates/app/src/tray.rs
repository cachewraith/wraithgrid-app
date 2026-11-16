//! The tray icon (`keepInTray` in app.ts): shown while the window is closed but agents (or
//! scheduled jobs) keep the app running. Its menu says how many agents run and opens the
//! window again or quits; a click opens the window. Linux speaks StatusNotifierItem over D-Bus
//! (`ksni`), as Electron's Linux tray does; elsewhere there is no tray yet, so the window
//! stays (the caller falls back to quitting with agents kept).

use futures::channel::mpsc::UnboundedSender;

/// What the tray asks the window side to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Sent from the Linux tray only, for now.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum TrayCommand {
    Open,
    StopAndQuit,
    QuitKeep,
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{TrayCommand, UnboundedSender};
    use ksni::blocking::{Handle, TrayMethods};
    use ksni::menu::{MenuItem, StandardItem};
    use ksni::{Icon, ToolTip, Tray as KsniTray};

    struct WgTray {
        live: usize,
        tx: UnboundedSender<TrayCommand>,
        icon: Vec<Icon>,
    }

    impl WgTray {
        fn send(&self, c: TrayCommand) {
            let _ = self.tx.unbounded_send(c);
        }
    }

    impl KsniTray for WgTray {
        fn id(&self) -> String {
            "wraithgrid".into()
        }

        fn title(&self) -> String {
            "Wraithgrid".into()
        }

        fn icon_name(&self) -> String {
            "wraithgrid".into()
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            self.icon.clone()
        }

        fn tool_tip(&self) -> ToolTip {
            ToolTip {
                title: "Wraithgrid: agents running".into(),
                ..Default::default()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayCommand::Open);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let item = |label: &str, c: TrayCommand| {
                StandardItem {
                    label: label.into(),
                    activate: Box::new(move |t: &mut Self| t.send(c)),
                    ..Default::default()
                }
                .into()
            };
            vec![
                StandardItem {
                    label: format!(
                        "{} agent{} running",
                        self.live,
                        if self.live == 1 { "" } else { "s" }
                    ),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                item("Open Wraithgrid", TrayCommand::Open),
                item("Stop all agents and quit", TrayCommand::StopAndQuit),
                item("Quit, keep agents running", TrayCommand::QuitKeep),
            ]
        }
    }

    /// The app icon at 22×22 (Electron's Linux tray size), ARGB in network order.
    fn icon() -> Vec<Icon> {
        const PNG: &[u8] = include_bytes!("../../../../build/icon.png");
        const SIZE: usize = 22;
        let decoder = png::Decoder::new(std::io::Cursor::new(PNG));
        let Ok(mut reader) = decoder.read_info() else {
            return vec![];
        };
        let mut buf = vec![0; reader.output_buffer_size()];
        let Ok(info) = reader.next_frame(&mut buf) else {
            return vec![];
        };
        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return vec![];
        }
        let (w, h) = (info.width as usize, info.height as usize);
        let mut data = Vec::with_capacity(SIZE * SIZE * 4);
        // Box filter: each tray pixel averages its square of source pixels.
        for ty in 0..SIZE {
            for tx in 0..SIZE {
                let (x0, x1) = (tx * w / SIZE, ((tx + 1) * w / SIZE).max(tx * w / SIZE + 1));
                let (y0, y1) = (ty * h / SIZE, ((ty + 1) * h / SIZE).max(ty * h / SIZE + 1));
                let mut sum = [0u32; 4];
                for y in y0..y1 {
                    for x in x0..x1 {
                        let p = &buf[(y * w + x) * 4..][..4];
                        for (s, v) in sum.iter_mut().zip(p) {
                            *s += u32::from(*v);
                        }
                    }
                }
                let n = ((x1 - x0) * (y1 - y0)) as u32;
                let [r, g, b, a] = sum.map(|s| (s / n) as u8);
                data.extend_from_slice(&[a, r, g, b]);
            }
        }
        vec![Icon {
            width: SIZE as i32,
            height: SIZE as i32,
            data,
        }]
    }

    pub struct Tray(Handle<WgTray>);

    impl Tray {
        pub fn show(tx: UnboundedSender<TrayCommand>) -> Option<Tray> {
            let tray = WgTray {
                live: 0,
                tx,
                icon: icon(),
            };
            // Like Electron's tray: made even when no tray host is listening yet.
            match tray.assume_sni_available(true).spawn() {
                Ok(h) => Some(Tray(h)),
                Err(e) => {
                    eprintln!("[wraithgrid] tray: {e}");
                    None
                }
            }
        }

        pub fn set_live(&self, live: usize) {
            self.0.update(|t| t.live = live);
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            drop(self.0.shutdown());
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{TrayCommand, UnboundedSender};

    pub struct Tray;

    impl Tray {
        pub fn show(_tx: UnboundedSender<TrayCommand>) -> Option<Tray> {
            None
        }

        pub fn set_live(&self, _live: usize) {}
    }
}

pub use imp::Tray;
