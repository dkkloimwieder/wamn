//! Hidden interactive enrollment and login through the shared terminal driver.

use std::error::Error;
use std::io::{self, IsTerminal as _};
use std::sync::Arc;

use crossterm::{
    ExecutableCommand as _,
    event::{EnableBracketedPaste, Event, EventStream, KeyCode, KeyEventKind, KeyModifiers},
};
use futures_util::StreamExt as _;
use ratatui::widgets::{Paragraph, Wrap};
use wamn_client::Transport;
use wamn_client::credentials::{PasswordCredentials, SecretInput, SessionTarget};
use zeroize::Zeroizing;

use crate::{
    TerminalSession,
    operator::{ExitReason, shutdown_signal},
};

/// The selected environment and its session, or a terminal exit request.
#[derive(Debug)]
pub enum PasswordLogin {
    Authenticated {
        index: usize,
        credentials: Arc<PasswordCredentials>,
    },
    Exit(ExitReason),
}

/// Accept an optional invitation, then prompt for an explicit password login.
///
/// # Errors
/// Refuses redirected input and failed enrollment/login without printing secrets.
/// An exit outcome means the operator cancelled before application startup.
pub async fn password_login(
    issuer: &str,
    audiences: &[String],
    transport: Arc<dyn Transport>,
) -> Result<PasswordLogin, Box<dyn Error>> {
    let first = audiences
        .first()
        .ok_or_else(|| io::Error::other("configure at least one environment"))?;
    let credentials =
        PasswordCredentials::new(SessionTarget::new(issuer, first)?, transport.clone());
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::other("password login requires an interactive terminal").into());
    }
    let shutdown = shutdown_signal()?;
    tokio::pin!(shutdown);
    let mut terminal = TerminalSession::enter()?;
    io::stdout().execute(EnableBracketedPaste)?;
    let mut events = crate::events();
    let selected = "Receiving sign-in";
    let flow = async {
        let choice = prompt(
            &mut terminal,
            &mut events,
            selected,
            "Enter L to log in, or I to accept an invitation:",
        )
        .await?;
        match choice.expose().to_ascii_lowercase().as_str() {
            "i" => {
                let principal = prompt(
                    &mut terminal,
                    &mut events,
                    selected,
                    "Principal ID from the invitation:",
                )
                .await?;
                let invitation = prompt(
                    &mut terminal,
                    &mut events,
                    selected,
                    "Invitation secret (hidden):",
                )
                .await?;
                let password = prompt(
                    &mut terminal,
                    &mut events,
                    selected,
                    "New password (hidden, at least 15 characters):",
                )
                .await?;
                let confirmation = prompt(
                    &mut terminal,
                    &mut events,
                    selected,
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
        let email = prompt(&mut terminal, &mut events, selected, "Email:").await?;
        let password = prompt(&mut terminal, &mut events, selected, "Password (hidden):").await?;
        let authorized = credentials.environments(email.expose(), &password).await?;
        let choices: Vec<_> = audiences
            .iter()
            .enumerate()
            .filter(|(_, audience)| authorized.contains(audience))
            .collect();
        let index = match choices.as_slice() {
            [] => {
                return Err(io::Error::other(
                    "no authorized environment matches this deployment configuration",
                )
                .into());
            }
            [(index, _)] => *index,
            _ => {
                let list = choices
                    .iter()
                    .enumerate()
                    .map(|(number, (_, audience))| format!("{}. {}", number + 1, audience))
                    .collect::<Vec<_>>()
                    .join("\n");
                let choice = prompt(
                    &mut terminal,
                    &mut events,
                    &list,
                    "Choose an environment number:",
                )
                .await?;
                let number = choice
                    .expose()
                    .parse::<usize>()
                    .ok()
                    .and_then(|value| value.checked_sub(1));
                number
                    .and_then(|number| choices.get(number))
                    .map(|(index, _)| *index)
                    .ok_or_else(|| io::Error::other("environment selection refused"))?
            }
        };
        let credentials =
            PasswordCredentials::new(SessionTarget::new(issuer, &audiences[index])?, transport);
        credentials.login(email.expose(), password).await?;
        Ok::<_, Box<dyn Error>>(PasswordLogin::Authenticated {
            index,
            credentials: Arc::new(credentials),
        })
    };
    tokio::select! {
        result = flow => match result {
            Err(error) if error.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::Interrupted) => Ok(PasswordLogin::Exit(ExitReason::Operator)),
            result => result,
        },
        result = &mut shutdown => Ok(PasswordLogin::Exit(result?)),
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
