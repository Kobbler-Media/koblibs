use crate::vendor::nntp::Error;

pub enum Failure {
    Content(String),
    Infrastructure(String),
}

impl From<String> for Failure {
    fn from(reason: String) -> Self {
        Self::Content(reason)
    }
}

impl Failure {
    pub fn io(context: &str, error: impl std::fmt::Display) -> Self {
        Self::Infrastructure(format!("{context}: {error}"))
    }

    pub fn providers(error: &Error) -> Self {
        let reason = format!("providers unavailable: {error}");
        if content(error) {
            Self::Content(reason)
        } else {
            Self::Infrastructure(reason)
        }
    }
}

const fn content(error: &Error) -> bool {
    matches!(
        error,
        Error::NotFound
            | Error::CrcMismatch { .. }
            | Error::Yenc(_)
            | Error::TooLarge
            | Error::InvalidArgument(_)
    )
}
