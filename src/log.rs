use std::fmt::Display;

/// Tiny stderr logger. Diagnostics go to stderr so command output such as
/// `status` and `config` stays pipeable.
#[derive(Clone, Copy, Debug)]
pub struct Log {
    verbose: bool,
}

impl Log {
    pub fn new(verbose: bool) -> Self {
        Self { verbose }
    }

    pub fn info(&self, message: impl Display) {
        eprintln!("wallpaper-guardian: {message}");
    }

    pub fn debug(&self, message: impl Display) {
        if self.verbose {
            eprintln!("wallpaper-guardian: {message}");
        }
    }
}
