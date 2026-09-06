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

use std::io::{self, Stdout, Write as _};
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
        let mut output = io::stdout();
        output.execute(EnterAlternateScreen)?;
        output.execute(Hide)?;
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(output))?,
        })
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
    let mut output = io::stdout();
    output.execute(Show)?;
    output.execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;
    output.flush()
}
