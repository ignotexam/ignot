//! The `ignot` command line.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod args;
mod commands;
mod files;
mod net;
mod run;

use std::io::{self, ErrorKind, Write};
use std::process::ExitCode;

use clap::Parser;

/// Whether a character can change how the text around it is shown: a control character, a line break that is
/// not a line feed, or one of the invisible marks that reorder or hide text.
fn misleads(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{61c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

/// Text made safe to show on a terminal: every character that [`misleads`] becomes `?`, but for a line feed
/// or a tab.
///
/// Much of what a command prints comes from files and replies written by others. Left as it is, such text
/// could carry escape sequences that move the cursor or repaint what was printed before, or marks that show
/// its letters in another order, and so make a failed check read like a passed one.
fn printable(text: &str) -> String {
    text.chars().map(|c| if misleads(c) && c != '\n' && c != '\t' { '?' } else { c }).collect()
}

/// Text written by someone else, as a part of one line of ours: [`printable`], and without a line break of its
/// own, so that it cannot add a line that reads like one of ours.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' | '\t' => ' ',
            c if misleads(c) => '?',
            c => c,
        })
        .collect()
}

/// Standard output as the commands use it: made [`printable`], and outliving its reader. When whoever was
/// reading goes away, as in `ignot verify-all | head`, the rest is dropped and the command still runs to its
/// end, so that its exit code stays true.
struct Terminal<W> {
    inner: W,
    gone: bool,
}

impl<W: Write> Write for Terminal<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.gone {
            match self.inner.write_all(printable(&String::from_utf8_lossy(buf)).as_bytes()) {
                Err(e) if e.kind() == ErrorKind::BrokenPipe => self.gone = true,
                outcome => outcome?,
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.gone {
            match self.inner.flush() {
                Err(e) if e.kind() == ErrorKind::BrokenPipe => self.gone = true,
                outcome => outcome?,
            }
        }
        Ok(())
    }
}

/// Exits with 0 when the command did its work, 1 when an exam failed its checks or a model that was to be
/// asked could not be reached, and 2 on any error.
fn main() -> ExitCode {
    let cli = args::Cli::parse();
    let api_key = std::env::var("OPENROUTER_API_KEY").ok();
    let mut out = Terminal { inner: io::stdout(), gone: false };
    match commands::run(cli.command, &net::Internet::new(), api_key.as_deref(), ignot_core::chain::RECORD, &mut out) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            // Not `eprintln!`, which panics when nobody reads the errors any more.
            writeln!(io::stderr(), "ignot: {}", one_line(&format!("{e:#}"))).ok();
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that takes a few bytes and then fails.
    struct Leaving {
        taken: Vec<u8>,
        room: usize,
        kind: ErrorKind,
    }

    impl Write for Leaving {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.taken.len() + buf.len() > self.room {
                return Err(self.kind.into());
            }
            self.taken.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn reader(room: usize, kind: ErrorKind) -> Terminal<Leaving> {
        Terminal { inner: Leaving { taken: Vec::new(), room, kind }, gone: false }
    }

    #[test]
    fn a_reader_that_leaves_does_not_stop_the_command() {
        let mut out = reader(8, ErrorKind::BrokenPipe);
        writeln!(out, "PASS a").unwrap();
        writeln!(out, "FAIL b, which nobody reads").unwrap();
        writeln!(out, "FAIL c").unwrap();
        out.flush().unwrap();
        assert!(out.gone);
        assert_eq!(out.inner.taken, b"PASS a\n");
    }

    #[test]
    fn control_characters_never_reach_the_terminal() {
        // An escape sequence in a published file must not get to repaint a line that says FAIL.
        let mut out = reader(1000, ErrorKind::BrokenPipe);
        writeln!(out, "FAIL x: unknown field `\u{1b}[1A\u{1b}[2KPASS`, tab\there, bell\u{7}, csi\u{9b}31m, del\u{7f}")
            .unwrap();
        assert_eq!(out.inner.taken, b"FAIL x: unknown field `?[1A?[2KPASS`, tab\there, bell?, csi?31m, del?\n");
        assert_eq!(printable("plain text, caf\u{e9}, 2 lines\n"), "plain text, caf\u{e9}, 2 lines\n");
    }

    #[test]
    fn marks_that_reorder_or_hide_text_never_reach_the_terminal() {
        // Shown as written, "\u{202e}SSAP" reads PASS, and a line separator starts a line of its own.
        assert_eq!(printable("FAIL \u{202e}SSAP\u{202c} x\u{2028}PASS y"), "FAIL ?SSAP? x?PASS y");
        let hidden = "\u{61c}\u{200b}\u{200f}\u{2029}\u{2060}\u{2064}\u{2066}\u{2069}\u{feff}\u{85}";
        assert_eq!(printable(hidden), "?".repeat(10));
        // Letters of every script, and the marks that join them, pass.
        let plain = "caf\u{e9} \u{4e2d}\u{6587} \u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629} \u{1f600} a\u{301}";
        assert_eq!(printable(plain), plain);
        assert_eq!(one_line(plain), plain);
    }

    #[test]
    fn text_from_others_stays_on_the_line_it_is_put_in() {
        assert_eq!(one_line("no such file\nPASS exams/pilot (open)"), "no such file PASS exams/pilot (open)");
        assert_eq!(one_line("a\r\nb\tc\u{2028}d\u{1b}[2K"), "a? b c?d?[2K");
    }

    #[test]
    fn any_other_failure_to_write_is_still_an_error() {
        let mut out = reader(0, ErrorKind::PermissionDenied);
        assert!(writeln!(out, "PASS a").is_err());
        assert!(!out.gone);
    }
}
