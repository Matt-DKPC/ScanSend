//! What happens on the computer when a scan arrives: type it like a USB
//! barcode scanner (keyboard wedge) and/or put it on the clipboard.
//!
//! Both run on one dedicated OS thread: the keyboard/clipboard handles are
//! not `Send`, and on Linux the clipboard contents only live as long as the
//! process holding them, so we keep one `Clipboard` alive forever.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::sync::{mpsc, Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Suffix {
    /// Press Enter after typing (what most USB scanners do).
    Enter,
    /// Press Tab after typing (jump to the next form field).
    Tab,
    /// Type the text only.
    None,
}

impl Suffix {
    pub fn label(self) -> &'static str {
        match self {
            Suffix::Enter => "Press Enter",
            Suffix::Tab => "Press Tab",
            Suffix::None => "Nothing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ActionOpts {
    pub type_text: bool,
    pub clipboard: bool,
    pub suffix: Suffix,
}

impl Default for ActionOpts {
    fn default() -> Self {
        ActionOpts { type_text: true, clipboard: true, suffix: Suffix::Enter }
    }
}

/// Settings shared with the GUI; read fresh for every scan.
pub type SharedOpts = Arc<Mutex<ActionOpts>>;

/// Text that is safe to inject as keystrokes: newlines become spaces and
/// other control characters (e.g. the GS1 group separator) are dropped.
pub fn typeable(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\r' => None,
            '\n' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn spawn(opts: SharedOpts) -> mpsc::Sender<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("scan-actions".into())
        .spawn(move || run(opts, rx))
        .expect("spawn action thread");
    tx
}

fn run(shared: SharedOpts, rx: mpsc::Receiver<String>) {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};

    let mut clipboard: Option<arboard::Clipboard> = None;
    let mut enigo: Option<Enigo> = None;

    for text in rx {
        let opts = *shared.lock().unwrap();
        if opts.clipboard {
            if clipboard.is_none() {
                match arboard::Clipboard::new() {
                    Ok(c) => clipboard = Some(c),
                    Err(e) => eprintln!("  ! clipboard unavailable: {e}"),
                }
            }
            if let Some(c) = clipboard.as_mut() {
                let clean: String = text.chars().filter(|&c| c != '\0').collect();
                if let Err(e) = c.set_text(clean) {
                    eprintln!("  ! could not copy to clipboard: {e}");
                    clipboard = None; // reconnect next time
                }
            }
        }

        if opts.type_text {
            if enigo.is_none() {
                match Enigo::new(&Settings::default()) {
                    Ok(e) => enigo = Some(e),
                    Err(e) => eprintln!("  ! cannot simulate keyboard: {e}"),
                }
            }
            if let Some(e) = enigo.as_mut() {
                let t = typeable(&text);
                let mut res = e.text(&t);
                if res.is_ok() {
                    res = match opts.suffix {
                        Suffix::Enter => e.key(Key::Return, Direction::Click),
                        Suffix::Tab => e.key(Key::Tab, Direction::Click),
                        Suffix::None => Ok(()),
                    };
                }
                if let Err(err) = res {
                    eprintln!("  ! typing failed: {err}");
                    enigo = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::typeable;

    #[test]
    fn strips_controls() {
        assert_eq!(typeable("0101234\u{1d}17260101\r\n"), "010123417260101");
        assert_eq!(typeable("line one\nline two"), "line one line two");
        assert_eq!(typeable("https://example.com/?a=1"), "https://example.com/?a=1");
    }
}
