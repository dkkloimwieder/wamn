//! Hidden interactive enrollment and login through the shared terminal driver.

use std::error::Error;
use std::io::{self, IsTerminal as _};

use crossterm::{
    ExecutableCommand as _,
    event::{EnableBracketedPaste, Event, EventStream, KeyCode, KeyEventKind, KeyModifiers},
};
use futures_util::StreamExt as _;
use ratatui::widgets::{Paragraph, Wrap};
use wamn_client::credentials::{PasswordCredentials, SecretInput};
use zeroize::Zeroizing;

use crate::{
    TerminalSession,
    operator::{ExitReason, shutdown_signal},
};

/// Accept an optional invitation, then prompt for an explicit password login.
///
/// # Errors
/// Refuses redirected input and failed enrollment/login without printing secrets.
/// A returned exit reason means the operator cancelled before application startup.
pub async fn password_login(
    credentials: &PasswordCredentials,
) -> Result<Option<ExitReason>, Box<dyn Error>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other("password login requires an interactive terminal").into());
    }
    let shutdown = shutdown_signal()?;
    tokio::pin!(shutdown);
    let mut terminal = TerminalSession::enter()?;
    io::stdout().execute(EnableBracketedPaste)?;
    let mut events = crate::events();
    let selected = format!("Environment: {}", credentials.audience());
    let flow = async {
        let choice = prompt(
            &mut terminal,
            &mut events,
            &selected,
            "Enter L to log in, or I to accept an invitation:",
        )
        .await?;
        match choice.expose().to_ascii_lowercase().as_str() {
            "i" => {
                let principal = prompt(
                    &mut terminal,
                    &mut events,
                    &selected,
                    "Principal ID from the invitation:",
                )
                .await?;
                let invitation = prompt(
                    &mut terminal,
                    &mut events,
                    &selected,
                    "Invitation secret (hidden):",
                )
                .await?;
                let password = prompt(
                    &mut terminal,
                    &mut events,
                    &selected,
                    "New password (hidden, at least 15 characters):",
                )
                .await?;
                let confirmation = prompt(
                    &mut terminal,
                    &mut events,
                    &selected,
                    "Confirm new password (hidden):",
                )
                .await?;
                if password.expose() != confirmation.expose() {
                    return Err(io::Error::other("password confirmation did not match").into());
                }
                drop(confirmation);
                credentials
                    .enroll(principal.expose(), invitation, password)
                    .await?;
            }
            "l" => (),
            _ => return Err(io::Error::other("enter L or I to select authentication").into()),
        }
        let email = prompt(&mut terminal, &mut events, &selected, "Email:").await?;
        let password = prompt(&mut terminal, &mut events, &selected, "Password (hidden):").await?;
        credentials.login(email.expose(), password).await?;
        Ok::<_, Box<dyn Error>>(None)
    };
    tokio::select! {
        result = flow => match result {
            Err(error) if error.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::Interrupted) => Ok(Some(ExitReason::Operator)),
            result => result,
        },
        result = &mut shutdown => Ok(Some(result?)),
    }
}

async fn prompt(
    terminal: &mut TerminalSession,
    events: &mut EventStream,
    selected: &str,
    label: &str,
) -> io::Result<SecretInput> {
    let mut input = Zeroizing::new(String::new());
    terminal.draw(
        Paragraph::new(format!(
            "{selected}\n\n{label}\n\nInput is hidden. Enter submits. Esc or Ctrl-C cancels."
        ))
        .wrap(Wrap { trim: false }),
    )?;
    loop {
        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "login cancelled",
                    ));
                }
                KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "login cancelled",
                    ));
                }
                KeyCode::Enter => {
                    return SecretInput::new(std::mem::take(&mut *input))
                        .map_err(|_| io::Error::other("input must contain 1 to 1024 bytes"));
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if input.len() + value.len_utf8() > 1024 {
                        return Err(io::Error::other("input exceeds 1024 bytes"));
                    }
                    input.push(value);
                }
                _ => (),
            },
            Some(Ok(Event::Paste(value))) => {
                let value = Zeroizing::new(value);
                if input.len() + value.len() > 1024 {
                    return Err(io::Error::other("input exceeds 1024 bytes"));
                }
                input.push_str(&value);
            }
            Some(Err(error)) => return Err(error),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "login input closed",
                ));
            }
            Some(Ok(_)) => (),
        }
    }
}
