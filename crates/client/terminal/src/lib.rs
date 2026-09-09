//! The terminal itself: raw mode, the alternate screen, events, restoration.
//!
//! Nothing here knows about the application above it. It was the private
//! `terminal` module of the developer client in `services/ctl/src/dev/tui.rs`
//! and moved out unchanged when the second caller arrived — the
//! `wamn-receiving-tui` binary — so the two share one driver instead of
//! carrying two copies of raw mode and two panic hooks (wamn-10yt.5.9).
//!
//! A widget is all a caller supplies. Everything a terminal client must not
//! get wrong — entering raw mode and the alternate screen in order, leaving
//! them in the reverse order, and leaving them even when the process panics —
//! happens here and nowhere else.

pub mod operator;

use std::io::{self, Stdout};
use std::panic;
use std::sync::Once;

use crossterm::ExecutableCommand as _;
use crossterm::cursor::{Hide, Show};
use crossterm::event::EventStream;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::Widget;

/// Guards the one process-wide panic hook this driver installs.
static PANIC_RESTORE: Once = Once::new();

/// Keyboard, mouse and resize events from the entered terminal.
pub fn events() -> EventStream {
    EventStream::new()
}

/// An entered terminal, restored when it is dropped or the process panics.
#[derive(Debug)]
pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    /// Enter raw mode and the alternate screen.
    ///
    /// # Errors
    ///
    /// The terminal refused raw mode, the alternate screen, or the initial
    /// backend query.
    pub fn enter() -> io::Result<Self> {
        PANIC_RESTORE.call_once(|| {
            let previous = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                // A panic under raw mode otherwise leaves the developer
                // with an unusable terminal and no visible message.
                drop(restore());
                previous(info);
            }));
        });
        enable_raw_mode()?;
        let entered = (|| {
            let mut output = io::stdout();
            output.execute(EnterAlternateScreen)?;
            output.execute(Hide)?;
            Ok(Self {
                terminal: Terminal::new(CrosstermBackend::new(output))?,
            })
        })();
        if entered.is_err() {
            drop(restore());
        }
        entered
    }

    /// Paint one widget over the whole terminal.
    ///
    /// # Errors
    ///
    /// The terminal refused the write.
    pub fn draw(&mut self, widget: impl Widget) -> io::Result<()> {
        self.terminal
            .draw(|frame| frame.render_widget(widget, frame.area()))?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        drop(restore());
    }
}

/// Leave the alternate screen and raw mode, in the reverse of entry order.
fn restore() -> io::Result<()> {
    restore_with(&mut io::stdout(), disable_raw_mode)
}

fn restore_with(
    output: &mut impl io::Write,
    disable_raw: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    // Run every cleanup step before returning the first error.
    let show = output.execute(Show).map(|_| ());
    let leave = output.execute(LeaveAlternateScreen).map(|_| ());
    let raw = disable_raw();
    let flush = output.flush();
    show.and(leave).and(raw).and(flush)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_output_still_disables_raw_mode_and_preserves_the_first_error() {
        #[derive(Default)]
        struct BrokenOutput {
            writes: usize,
            flushed: bool,
        }
        impl io::Write for BrokenOutput {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.writes += 1;
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "screen write failed",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushed = true;
                Err(io::Error::other("flush failed"))
            }
        }
        let mut output = BrokenOutput::default();
        let mut disabled = false;
        let error = restore_with(&mut output, || {
            disabled = true;
            Err(io::Error::other("raw mode failed"))
        })
        .unwrap_err();
        assert!(disabled);
        assert!(output.writes >= 2);
        assert!(output.flushed);
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), "screen write failed");
    }
}
