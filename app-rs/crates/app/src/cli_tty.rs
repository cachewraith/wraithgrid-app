//! The local terminal for `wraithgrid attach` (H7): raw mode, size and keyboard input.

use std::io::{Read, Write};
use std::sync::mpsc;

use wraithgrid_core::cli::Tty;

/// stdin/stdout when both are terminals; None otherwise (attach then refuses).
pub fn local_tty() -> Option<Box<dyn Tty>> {
    #[cfg(unix)]
    {
        // SAFETY: isatty only inspects the fds.
        let is_tty = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
        is_tty.then(|| Box::new(UnixTty { saved: None }) as Box<dyn Tty>)
    }
    #[cfg(not(unix))]
    None
}

#[cfg(unix)]
struct UnixTty {
    saved: Option<libc::termios>,
}

#[cfg(unix)]
impl Tty for UnixTty {
    fn size(&self) -> (u16, u16) {
        // SAFETY: TIOCGWINSZ fills the winsize struct for a terminal fd.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0
            && ws.ws_col > 0
            && ws.ws_row > 0
        {
            (ws.ws_col, ws.ws_row)
        } else {
            (80, 24)
        }
    }

    fn write(&mut self, data: &str) {
        let mut out = std::io::stdout();
        let _ = out.write_all(data.as_bytes());
        let _ = out.flush();
    }

    fn raw(&mut self, on: bool) {
        // SAFETY: termios calls on stdin, restoring the saved state when turned off.
        unsafe {
            if on {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut t) == 0 {
                    self.saved = Some(t);
                    libc::cfmakeraw(&mut t);
                    libc::tcsetattr(0, libc::TCSANOW, &t);
                }
            } else if let Some(t) = self.saved.take() {
                libc::tcsetattr(0, libc::TCSANOW, &t);
            }
        }
    }

    fn input(&mut self) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0
                    || tx
                        .send(String::from_utf8_lossy(&buf[..n]).into_owned())
                        .is_err()
                {
                    break;
                }
            }
        });
        rx
    }
}

#[cfg(unix)]
impl Drop for UnixTty {
    fn drop(&mut self) {
        self.raw(false);
    }
}
