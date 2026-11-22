use std::sync::Arc;

use crate::ImportMode;
use crate::jobs::failure::Failure;
use crate::jobs::fetch::Fetcher;
use crate::jobs::store::{Plan, Planned};
use crate::repair::recovery;
use crate::vendor::queue::import::{
    self, Error as ImportError, Imported, NameSource, Progress, Rules,
};
use crate::vendor::queue::nzb::Nzb;

pub enum Next {
    Plan(Plan),
    Raw,
    Nested(Vec<u8>),
    Fail(Failure),
}

pub async fn direct(fetcher: &Fetcher, nzb: &Nzb, name: &str, password: Option<&str>) -> Next {
    let rules = Rules {
        check_percent: 0,
        concurrency: ImportMode::Download.concurrency(),
        ..Rules::default()
    };
    let imported = import::import(
        &fetcher.client,
        nzb,
        name,
        "",
        &rules,
        password,
        Arc::new(Progress::default()),
        None,
    )
    .await;
    match imported {
        Ok(Imported::Files { outputs, .. }) if outputs.iter().any(|o| o.meta.encrypted()) => {
            Next::Raw
        }
        Ok(Imported::Files { outputs, .. }) => Next::Plan(Plan {
            raw: false,
            probed: false,
            outputs: outputs
                .into_iter()
                .map(|o| Planned {
                    path: o.path.join("/"),
                    member: o.origin.source == NameSource::ArchiveHeader,
                    crc32: o.crc32,
                    meta: o.meta,
                })
                .collect(),
        }),
        Ok(Imported::Nzb(inner)) => Next::Nested(inner),
        Err(ImportError::Password(reason)) => {
            Next::Fail(Failure::Content(password_failure(password, &reason)))
        }
        Err(ImportError::Unsupported(reason)) => {
            tracing::info!(%reason, "downloading whole volumes to unpack them");
            Next::Raw
        }
        Err(ImportError::Missing(reason)) if has_par2(nzb) => {
            tracing::info!(%reason, "downloading whole volumes to repair them");
            Next::Raw
        }
        Err(ImportError::Retry(error)) => Next::Fail(Failure::providers(&error)),
        Err(error) => Next::Fail(Failure::Content(error.to_string())),
    }
}

pub fn password_failure(password: Option<&str>, reason: &str) -> String {
    match password {
        None => format!(
            "The archive is password-protected and no password was given (NZB meta or job): {reason}"
        ),
        Some(_) => format!("Wrong archive password: {reason}"),
    }
}

pub fn has_par2(nzb: &Nzb) -> bool {
    recovery::par2_files(nzb).next().is_some()
}
