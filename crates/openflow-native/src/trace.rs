//! Diagnostics for the paths that have no other way to report.
//!
//! A menu bar app is opened with `open -a`, so there is no terminal attached and
//! no way to add a print to a build that is already running. The two paths that
//! failed in the first supervised smoke run -- a tray click, and a window being
//! told to come forward -- now say so on stderr, which `open -a` routes to the
//! unified log, but only when `OPENFLOW_TRACE=1` is in the environment. Set it
//! for a GUI launch with `launchctl setenv OPENFLOW_TRACE 1` before opening the
//! app, and `launchctl unsetenv OPENFLOW_TRACE` afterwards.
//!
//! Default is silent: the check is one `OnceLock` read, and nothing is
//! formatted unless the flag is on.

use std::sync::OnceLock;

/// Whether `value` turns tracing on. Opt-in and exact, so a variable that is
/// present but empty leaves the app silent.
fn wants_tracing(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Whether tracing was asked for. Read once: the environment of a running app
/// does not change under it.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| wants_tracing(std::env::var("OPENFLOW_TRACE").ok().as_deref()))
}

/// Write one trace line to stderr, or nothing at all.
///
/// Deliberately not `eprintln!`: that panics if stderr is gone, and a menu bar
/// app has nowhere to show a panic. `writeln!` hands back its error instead and
/// this drops it.
pub fn line(message: std::fmt::Arguments<'_>) {
    if !enabled() {
        return;
    }
    use std::io::Write;
    let stderr = std::io::stderr();
    let mut handle = stderr.lock();
    let _ = writeln!(handle, "[openflow] {}", message);
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        $crate::trace::line(::core::format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag is opt-in and exact. A build that logged on any non-empty value
    /// would start writing to the unified log for `OPENFLOW_TRACE=0`, which is
    /// how a user turns it off.
    #[test]
    fn tracing_is_off_unless_the_variable_is_exactly_one() {
        assert!(wants_tracing(Some("1")));
        assert!(!wants_tracing(Some("")));
        assert!(!wants_tracing(Some("0")));
        assert!(!wants_tracing(Some("true")));
        assert!(!wants_tracing(None));
    }
}
