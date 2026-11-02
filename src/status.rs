//! Cargo-style status lines on stderr (`    Checking ruby v1.0.0 (packs/ruby)`), so a run that is
//! busy says so.

use std::fmt::Display;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anstyle::{AnsiColor, Style};

static QUIET: AtomicBool = AtomicBool::new(false);

/// Leaves the status lines out (`--quiet`); diagnostics are still printed.
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

/// One status line: `verb`, right-aligned and in green, then `message`.
pub fn status(verb: &str, message: impl Display) {
    if QUIET.load(Ordering::Relaxed) {
        return;
    }
    let style = Style::new().bold().fg_color(Some(AnsiColor::Green.into()));
    anstream::eprintln!("{style}{verb:>12}{style:#} {message}");
}

/// A duration as cargo shows it: `0.84s`, `1m 03s`.
pub fn elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 60.0 {
        format!("{seconds:.2}s")
    } else {
        let whole = duration.as_secs();
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

/// `count` and `word`, with an `s` unless the count is one: `1 pack`, `3 packs`.
pub fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_cargo() {
        assert_eq!(elapsed(Duration::from_millis(840)), "0.84s");
        assert_eq!(elapsed(Duration::from_secs(63)), "1m 03s");
    }

    #[test]
    fn plural_adds_an_s_unless_one() {
        assert_eq!(plural(0, "pack"), "0 packs");
        assert_eq!(plural(1, "pack"), "1 pack");
        assert_eq!(plural(2, "vanilla pack"), "2 vanilla packs");
    }
}
