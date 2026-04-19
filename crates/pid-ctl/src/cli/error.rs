use std::fmt;

#[derive(Debug)]
pub(crate) struct CliError {
    pub(crate) exit_code: i32,
    pub(crate) message: String,
}

impl CliError {
    pub(crate) fn new(exit_code: i32, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            message: message.into(),
        }
    }

    pub(crate) fn config(message: impl Into<String>) -> Self {
        Self::new(3, message)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
