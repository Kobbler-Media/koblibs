use crate::vendor::nntp::Workload;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Purpose {
    Play,
    Cache,
    Warm,
}

impl Purpose {
    #[must_use]
    pub const fn workload(self) -> Workload {
        match self {
            Self::Play => Workload::Stream,
            Self::Cache => Workload::Cache,
            Self::Warm => Workload::Warm,
        }
    }
}
