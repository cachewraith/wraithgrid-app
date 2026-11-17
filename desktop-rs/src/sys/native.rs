//! Native pieces Electron gave the main process for free: a message box with buttons and a
//! checkbox (`dialog.showMessageBox`), and the clipboard's image (`clipboard.read()`). On
//! Linux both go through GTK on the main thread, where Tauri runs it; callers wait on a
//! channel, so call these from a background thread, never from the main thread.

use tauri::AppHandle;

/// What a message box answered: the index of the button, and the checkbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Answer {
    pub button: usize,
    pub checked: bool,
}

pub struct MessageBox<'a> {
    pub message: &'a str,
    pub detail: &'a str,
    pub buttons: &'a [&'a str],
    pub default: usize,
    /// The answer when the box is closed without a button.
    pub cancel: usize,
    pub checkbox: Option<&'a str>,
}

/// Shows a modal question over the main window (if it is up) and waits for the answer.
#[cfg(target_os = "linux")]
pub fn message_box(app: &AppHandle, m: MessageBox) -> Answer {
    use gtk::prelude::*;
    use tauri::Manager;

    let (tx, rx) = std::sync::mpsc::channel();
    let (message, detail, checkbox) = (
        m.message.to_string(),
        m.detail.to_string(),
        m.checkbox.map(str::to_string),
    );
    let buttons: Vec<String> = m.buttons.iter().map(|b| b.to_string()).collect();
    let (default, cancel) = (m.default, m.cancel);
    let handle = app.clone();
    let shown = app.run_on_main_thread(move || {
        let parent = handle
            .get_webview_window("main")
            .filter(|w| w.is_visible().unwrap_or(false))
            .and_then(|w| w.gtk_window().ok());
        let dialog = gtk::MessageDialog::new(
            parent.as_ref(),
            gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
            gtk::MessageType::Question,
            gtk::ButtonsType::None,
            &message,
        );
        dialog.set_title("Wraithgrid");
        dialog.set_secondary_text(Some(&detail));
        for (i, label) in buttons.iter().enumerate() {
            dialog.add_button(label, gtk::ResponseType::Other(i as u16));
        }
        dialog.set_default_response(gtk::ResponseType::Other(default as u16));
        let check = checkbox.map(|label| {
            let check = gtk::CheckButton::with_label(&label);
            if let Ok(area) = dialog.message_area().downcast::<gtk::Box>() {
                area.pack_start(&check, false, false, 0);
            }
            check.show();
            check
        });
        let response = dialog.run();
        let checked = check.is_some_and(|c| c.is_active());
        // SAFETY: the dialog is ours and nothing else holds it.
        unsafe { dialog.destroy() };
        let button = match response {
            gtk::ResponseType::Other(i) => i as usize,
            _ => cancel,
        };
        let _ = tx.send(Answer { button, checked });
    });
    if shown.is_err() {
        return Answer {
            button: m.cancel,
            checked: false,
        };
    }
    rx.recv().unwrap_or(Answer {
        button: m.cancel,
        checked: false,
    })
}

/// Elsewhere there is no native box yet: the default button.
#[cfg(not(target_os = "linux"))]
pub fn message_box(_app: &AppHandle, m: MessageBox) -> Answer {
    Answer {
        button: m.default,
        checked: false,
    }
}

/// Clipboard image types we save, best first, with the file extension each gets.
pub const IMAGE_TYPES: [(&str, &str); 5] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/webp", "webp"),
    ("image/gif", "gif"),
    ("image/bmp", "bmp"),
];

/// The first type we save among those a clipboard offers.
pub fn pick_image_type<'a>(
    offered: impl Iterator<Item = &'a str> + Clone,
) -> Option<(&'static str, &'static str)> {
    IMAGE_TYPES
        .iter()
        .find(|(mime, _)| offered.clone().any(|t| t.trim() == *mime))
        .copied()
}

/// The clipboard's image bytes and file extension, as GTK reads it (X11 or Wayland).
#[cfg(target_os = "linux")]
pub fn clipboard_image(app: &AppHandle) -> Option<(Vec<u8>, &'static str)> {
    use gtk::gdk;
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let clipboard = gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD);
        let targets: Vec<String> = clipboard
            .wait_for_targets()
            .unwrap_or_default()
            .iter()
            .map(|a| a.name().to_string())
            .collect();
        let found = pick_image_type(targets.iter().map(String::as_str)).and_then(|(mime, ext)| {
            let data = clipboard
                .wait_for_contents(&gdk::Atom::intern(mime))?
                .data();
            (!data.is_empty()).then_some((data, ext))
        });
        let _ = tx.send(found);
    })
    .ok()?;
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .ok()
        .flatten()
}

#[cfg(not(target_os = "linux"))]
pub fn clipboard_image(_app: &AppHandle) -> Option<(Vec<u8>, &'static str)> {
    None
}
