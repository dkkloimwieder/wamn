//! The serial source of the device loop: the host owns the port.
//!
//! The host opens the tty read-only, sets raw mode and the baud rate, and
//! reads frames that end in a newline. It drops a trailing carriage return. It
//! drops an empty frame, a frame that is not UTF-8, and a frame longer than
//! `max_frame`, and counts each dropped frame, because drops show a
//! misconfigured device. It logs the first drop only. The device operation
//! sees only the frame text. A device that the host must drive (write, poll, handshake) needs a
//! host plugin with a WIT import instead (docs/plan/edge.md 4.2).

use std::fs::File;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use rustix::fs::{Mode, OFlags};
use rustix::termios::{ControlModes, OptionalActions, SpecialCodeIndex};
use tokio::sync::mpsc;

use crate::config::SerialConfig;

/// How long one read waits for a byte, in tenths of a second, before the
/// reader looks whether the loop still wants frames.
const READ_WAIT_DECISECONDS: u8 = 1;

/// The frames of an open serial port, and the count of frames it dropped.
#[derive(Debug)]
pub struct SerialFrames {
    pub frames: mpsc::Receiver<String>,
    pub dropped: Arc<AtomicU64>,
}

/// Open the port of `config` and read its frames on a thread. The thread ends
/// when the receiver closes or the port fails.
///
/// The channel holds one frame, so frames that arrive during a call wait in
/// the tty buffer.
pub fn open(config: &SerialConfig) -> anyhow::Result<SerialFrames> {
    let port = open_raw(config)
        .with_context(|| format!("open the serial port {}", config.path.display()))?;
    let (frames, received) = mpsc::channel(1);
    let dropped = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&dropped);
    let max_frame = config.max_frame;
    std::thread::Builder::new()
        .name("wamn-edge-serial".to_owned())
        .spawn(move || read_frames(port, max_frame, &frames, &counter))
        .context("start the serial reader")?;
    Ok(SerialFrames {
        frames: received,
        dropped,
    })
}

/// Open the tty without making it the controlling terminal, and set raw mode,
/// the baud rate and a bounded read wait.
fn open_raw(config: &SerialConfig) -> anyhow::Result<File> {
    // A tty without CLOCAL can block its open on the carrier line, so the open
    // does not block, and the flag clears once CLOCAL is set.
    let port = rustix::fs::open(
        &config.path,
        OFlags::RDONLY | OFlags::NOCTTY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut termios = rustix::termios::tcgetattr(&port).context("read the tty mode")?;
    termios.make_raw();
    termios.control_modes |= ControlModes::CLOCAL | ControlModes::CREAD;
    termios
        .set_speed(config.baud)
        .with_context(|| format!("set the baud rate {}", config.baud))?;
    termios.special_codes[SpecialCodeIndex::VMIN] = 0;
    termios.special_codes[SpecialCodeIndex::VTIME] = READ_WAIT_DECISECONDS;
    rustix::termios::tcsetattr(&port, OptionalActions::Now, &termios)
        .context("set the tty mode")?;
    rustix::fs::fcntl_setfl(&port, OFlags::empty()).context("clear the non-blocking flag")?;
    Ok(File::from(port))
}

/// Send each frame of `port` to `frames` until the receiver closes or a read
/// fails, and count each dropped frame in `dropped`. A read that returns no
/// byte is a read wait that ended.
fn read_frames(
    mut port: impl Read,
    max_frame: usize,
    frames: &mpsc::Sender<String>,
    dropped: &AtomicU64,
) {
    let mut frame = Vec::with_capacity(max_frame);
    let mut too_long = false;
    let mut buffer = [0; 256];
    while !frames.is_closed() {
        let read = match port.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                tracing::warn!(%error, "the serial port failed; the device loop ends");
                return;
            }
        };
        for &byte in &buffer[..read] {
            if byte != b'\n' {
                if frame.len() == max_frame {
                    too_long = true;
                } else if !too_long {
                    frame.push(byte);
                }
                continue;
            }
            let text = if too_long {
                Err("longer than max_frame")
            } else {
                frame_text(&frame)
            };
            match text {
                Ok(text) => {
                    if frames.blocking_send(text).is_err() {
                        return;
                    }
                }
                Err(reason) => drop_frame(dropped, reason),
            }
            frame.clear();
            too_long = false;
        }
    }
}

/// The text of one frame without its carriage return, or why it is dropped.
fn frame_text(frame: &[u8]) -> Result<String, &'static str> {
    let frame = frame.strip_suffix(b"\r").unwrap_or(frame);
    if frame.is_empty() {
        return Err("empty");
    }
    std::str::from_utf8(frame)
        .map(str::to_owned)
        .map_err(|_| "not UTF-8")
}

/// Count one dropped frame, and log it when it is the first.
fn drop_frame(dropped: &AtomicU64, reason: &'static str) {
    if dropped.fetch_add(1, Ordering::Relaxed) == 0 {
        tracing::warn!(
            reason,
            "the device sent a frame that the loop drops; the loop counts later drops and \
             does not log them"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::atomic::{AtomicU64, Ordering};

    use tokio::sync::mpsc;

    use super::read_frames;

    /// A port that returns its reads in order, then fails.
    struct Port(Vec<&'static [u8]>);

    impl Read for Port {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            let chunk = self.0.remove(0);
            buffer[..chunk.len()].copy_from_slice(chunk);
            Ok(chunk.len())
        }
    }

    #[test]
    fn frames_split_on_newlines_and_a_long_or_empty_frame_is_dropped() {
        let (sender, mut receiver) = mpsc::channel(8);
        let port = Port(vec![
            b"12.5 k",
            b"g\r\n\n",
            b"",
            b"0123456789ABC\nshort\n",
            b"\xff\n7\n",
        ]);
        let dropped = AtomicU64::new(0);
        read_frames(port, 8, &sender, &dropped);
        drop(sender);
        let mut frames = Vec::new();
        while let Some(frame) = receiver.blocking_recv() {
            frames.push(frame);
        }
        assert_eq!(frames, ["12.5 kg", "short", "7"]);
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            3,
            "the empty, long and non-UTF-8 frames"
        );
    }
}
