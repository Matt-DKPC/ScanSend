//! "Save scans to a CSV file": every scan becomes one row of
//! Documents/ScanSend/scans.csv (Date, Time, Phone, Format, Code).
//!
//! The file is opened, appended to and closed for each scan, so it can be
//! opened in Excel at any time. Excel locks a file it has open; rows that
//! can't be written then wait here and go in as soon as the file is free.

use crate::server::ScanEvent;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Why the last write failed (shown in Settings), cleared once a write works.
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().unwrap().clone()
}

pub fn path() -> PathBuf {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ScanSend")
        .join("scans.csv")
}

/// Rows waiting for the file, oldest first.
#[derive(Default)]
pub struct Writer {
    pending: Vec<String>,
}

impl Writer {
    pub fn add(&mut self, ev: &ScanEvent) {
        let now = chrono::Local::now();
        self.pending.push(row(&[
            &now.format("%Y-%m-%d").to_string(),
            &ev.time,
            &ev.device,
            &ev.format,
            &ev.text,
        ]));
        self.flush();
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let p = path();
        let res = (|| -> std::io::Result<()> {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let new = !p.exists();
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p)?;
            let mut out = String::new();
            if new {
                // The BOM makes Excel read the file as UTF-8 (names with emoji, accents).
                out.push('\u{feff}');
                out.push_str(&row(&["Date", "Time", "Phone", "Format", "Code"]));
            }
            out.extend(self.pending.iter().map(String::as_str));
            f.write_all(out.as_bytes())
        })();
        let mut err = LAST_ERROR.lock().unwrap();
        match res {
            Ok(()) => {
                self.pending.clear();
                *err = None;
            }
            Err(e) => {
                eprintln!("  ! could not save to {}: {e}", p.display());
                *err = Some(format!(
                    "Couldn't save to {} ({e}). If it's open in Excel, close it; {} waiting scan(s) will be saved then.",
                    p.display(),
                    self.pending.len()
                ));
            }
        }
    }
}

/// One CSV line (RFC 4180, CRLF like Excel writes).
fn row(fields: &[&str]) -> String {
    let mut line = fields.iter().map(|f| field(f)).collect::<Vec<_>>().join(",");
    line.push_str("\r\n");
    line
}

fn field(value: &str) -> String {
    let digits = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    let v = if digits && (value.starts_with('0') || value.len() > 15) {
        // Excel turns 012345678905 into 12345678905 (or 1.23E+11): keep barcodes as text.
        format!("=\"{value}\"")
    } else if value.starts_with(['=', '+', '-', '@', '\t', '\r']) && value.parse::<f64>().is_err() {
        // A QR code saying =HYPERLINK(...) must stay text, not become a formula.
        format!("'{value}")
    } else {
        value.to_string()
    };
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::{field, row};

    #[test]
    fn fields() {
        assert_eq!(field("ABC123"), "ABC123");
        assert_eq!(field("4006381333931"), "4006381333931");
        assert_eq!(field("012345678905"), "\"=\"\"012345678905\"\"\"");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("line\nbreak"), "\"line\nbreak\"");
        assert_eq!(field("=1+1"), "'=1+1");
        assert_eq!(field("-5"), "-5");
        assert_eq!(row(&["a", "b"]), "a,b\r\n");
    }
}
