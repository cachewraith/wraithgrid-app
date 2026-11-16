//! What screen readers see (AccessKit through GPUI): an element only shows up with an id and a
//! role, so clickable ones say what they are and what they do here, like Electron's
//! `aria-label`s.

use gpui_kit::*;

pub trait A11y: StatefulInteractiveElement + Sized {
    /// A button, named `label` (the text it shows, or what an icon-only one does).
    fn a11y_button(self, label: impl Into<SharedString>) -> Self {
        self.role(Role::Button).aria_label(label)
    }

    /// A button that is on or off (a chip, a toggle).
    fn a11y_toggle(self, label: impl Into<SharedString>, on: bool) -> Self {
        self.role(Role::Button)
            .aria_label(label)
            .aria_toggled(if on { Toggled::True } else { Toggled::False })
    }

    /// A dialog, named by its title.
    fn a11y_dialog(self, title: impl Into<SharedString>) -> Self {
        self.role(Role::Dialog).aria_label(title)
    }
}

impl<T: StatefulInteractiveElement> A11y for T {}
