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
        let choice = visible_prompt(
            &mut terminal,
            &mut events,
            selected,
            "Enter L to log in, I to accept an invitation, or R to reset your password:",
        )
        .await?;
        match choice.expose().to_ascii_lowercase().as_str() {
            "i" => {
                let invitation = prompt(
                    &mut terminal,
                    &mut events,
                    selected,
                    "Invitation code from your email (hidden):",
                )
                .await?;
                let (principal, secret) = invitation.expose().split_once(':').ok_or_else(|| {
                    io::Error::other("paste the complete invitation code from your email")
                })?;
                let principal = principal.to_owned();
                let secret = SecretInput::new(secret.to_owned())?;
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
                credentials.enroll(&principal, secret, password).await?;
            }
            "r" => {
                let email =
                    visible_prompt(&mut terminal, &mut events, selected, "Recovery email:").await?;
                terminal.draw(
                    Paragraph::new(
                        "Requesting recovery email. Please wait for the reset-code prompt.",
                    )
                    .wrap(Wrap { trim: false }),
                )?;
                credentials.recover(email.expose()).await?;
                let secret = prompt(&mut terminal, &mut events, selected, "If the account is eligible, a recovery email will arrive.\nReset secret from your email (hidden):").await?;
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
                let notified = credentials.reset(email.expose(), secret, password).await?;
                let message = if notified {
                    "Password changed. Enter L to sign in:"
                } else {
                    "Password changed. Notification delivery failed. Enter L to sign in:"
                };
                let choice = visible_prompt(&mut terminal, &mut events, selected, message).await?;
                if !choice.expose().eq_ignore_ascii_case("l") {
                    return Err(io::Error::other("enter L to sign in").into());
                }
            }
            "l" => (),
            _ => return Err(io::Error::other("enter L, I, or R to select authentication").into()),
        }
        let email = visible_prompt(&mut terminal, &mut events, selected, "Email:").await?;
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
                let choice = visible_prompt(
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
    input_prompt(terminal, events, selected, label, false).await
}
async fn visible_prompt(
    terminal: &mut TerminalSession,
    events: &mut EventStream,
    selected: &str,
    label: &str,
) -> io::Result<SecretInput> {
    input_prompt(terminal, events, selected, label, true).await
}

async fn input_prompt(
    terminal: &mut TerminalSession,
    events: &mut EventStream,
    selected: &str,
    label: &str,
    visible: bool,
) -> io::Result<SecretInput> {
    let mut input = Zeroizing::new(String::new());
    loop {
        let shown = if visible {
            input
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
        } else {
            "*".repeat(input.chars().count())
        };
        terminal.draw(
            Paragraph::new(format!(
                "{selected}\n\n{label}\n\n{shown}\n\nEnter submits. Esc or Ctrl-C cancels."
            ))
            .wrap(Wrap { trim: false }),
        )?;
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
                    if input.is_empty() {
                        continue;
                    }
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
