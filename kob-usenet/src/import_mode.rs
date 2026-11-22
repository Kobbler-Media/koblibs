#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    Stream,
    Download,
}

impl ImportMode {
    pub(crate) const fn check_percent(self) -> u8 {
        match self {
            Self::Stream => 0,
            Self::Download => 5,
        }
    }

    pub(crate) const fn concurrency(self) -> usize {
        match self {
            Self::Stream => 48,
            Self::Download => 16,
        }
    }
}
