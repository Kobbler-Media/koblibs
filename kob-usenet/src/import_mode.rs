use crate::vendor::nntp::Workload;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    Stream,
    Prewarm,
    Download,
}

impl ImportMode {
    pub(crate) const fn check_percent(self) -> u8 {
        match self {
            Self::Stream | Self::Prewarm => 0,
            Self::Download => 5,
        }
    }

    pub(crate) const fn workload(self) -> Workload {
        match self {
            Self::Stream => Workload::Import,
            Self::Prewarm => Workload::Warm,
            Self::Download => Workload::Queue,
        }
    }

    pub(crate) const fn concurrency(self) -> usize {
        match self {
            Self::Stream => 48,
            Self::Prewarm | Self::Download => 16,
        }
    }
}
