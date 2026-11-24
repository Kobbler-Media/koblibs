use std::sync::Arc;

use crate::jobs::failure::Failure;
use crate::jobs::fetch::Fetcher;
use crate::jobs::payload;
use crate::jobs::store::{Plan, Planned};
use crate::repair::recovery::Par2s;
use crate::vendor::queue::import::{
    self, Error as ImportError, Imported, NameSource, Progress, Rules,
};
use crate::vendor::queue::names;
use crate::vendor::queue::nzb::Nzb;

pub enum Next {
    Plan(Plan),
    Raw,
    Nested(Vec<u8>),
    Fail(Failure),
}

pub async fn direct(
    fetcher: &Fetcher,
    nzb: &Nzb,
    par2: &Par2s,
    name: &str,
    passwords: &[String],
) -> Next {
    let rules = Rules {
        check_percent: 0,
        concurrency: fetcher.planning_breadth(),
        ..Rules::default()
    };
    let release = names::sanitize(name).unwrap_or_else(|| "release".to_owned());
    let mut tried = 0;
    let imported = loop {
        let password = passwords.get(tried).map(String::as_str);
        let imported = import::import(
            &fetcher.client,
            nzb,
            &release,
            "",
            &rules,
            password,
            Arc::new(Progress::default()),
            None,
        )
        .await;
        tried += 1;
        match &imported {
            Err(ImportError::Password(_)) if tried < passwords.len() => {}
            _ => break imported,
        }
    };
    let password = passwords.get(tried - 1).map(String::as_str);
    let unproven = match &imported {
        Ok(Imported::Files { outputs, .. }) => !payload::proven(outputs),
        Err(ImportError::Password(_) | ImportError::Unsupported(_) | ImportError::Failed(_)) => {
            true
        }
        Ok(Imported::Nzb(_)) | Err(_) => false,
    };
    if unproven
        && let Some(refusal) =
            payload::only_executables(fetcher, nzb, &release, &rules, password).await
    {
        return Next::Fail(Failure::Content(refusal));
    }
    match imported {
        Ok(Imported::Files { outputs, .. }) if outputs.iter().any(|o| o.meta.encrypted()) => {
            Next::Raw
        }
        Ok(Imported::Files { outputs, .. }) => {
            let outputs: Vec<Planned> = outputs
                .into_iter()
                .map(|o| Planned {
                    path: o.path.join("/"),
                    member: o.origin.source == NameSource::ArchiveHeader,
                    crc32: o.crc32,
                    meta: o.meta,
                })
                .collect();
            if let Err(failure) = outputs.iter().try_for_each(Planned::contained) {
                return Next::Fail(failure);
            }
            Next::Plan(Plan {
                raw: false,
                probed: false,
                outputs,
            })
        }
        Ok(Imported::Nzb(inner)) => Next::Nested(inner),
        Err(ImportError::Password(reason)) => {
            Next::Fail(Failure::Content(password_failure(password, &reason)))
        }
        Err(ImportError::Unsupported(reason)) => {
            tracing::info!(%reason, "downloading whole volumes to unpack them");
            Next::Raw
        }
        Err(ImportError::Missing(reason)) if !par2.is_empty() => {
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
