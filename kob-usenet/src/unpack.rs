mod scan;
mod tool;
mod tree;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::jobs::failure::Failure;
use crate::space::SpaceGate;
use scan::{Family, Set};
use tool::Attempt;

const DEPTH: usize = 3;

pub struct Unpack<'a> {
    pub unrar: &'a Path,
    pub sevenzip: &'a Path,
    pub passwords: &'a [String],
    pub space: &'a SpaceGate,
    pub stall: Duration,
}

impl Unpack<'_> {
    pub async fn run(
        &self,
        dir: &Path,
        mut extracted: Vec<String>,
        record: &(dyn Fn(&[String]) -> Result<(), Failure> + Sync),
    ) -> Result<(), Failure> {
        scan::sweep(dir).map_err(|e| Failure::io(&format!("cleaning {}", dir.display()), e))?;
        for _ in 0..DEPTH {
            let sets = scan::sets(dir)
                .map_err(|e| Failure::io(&format!("listing {}", dir.display()), e))?;
            if sets.is_empty() {
                break;
            }
            for set in sets {
                let key = key(dir, &set.volumes[0]);
                if !extracted.contains(&key) {
                    self.unpack(&set).await?;
                    extracted.push(key);
                    record(&extracted)?;
                }
                remove(&set.volumes)?;
            }
        }
        Ok(())
    }

    async fn unpack(&self, set: &Set) -> Result<(), Failure> {
        let parent = set.volumes[0].parent().unwrap_or_else(|| Path::new("."));
        if set.family == Family::Split {
            return tree::join(parent, &set.name, &set.volumes)
                .map_err(|e| Failure::io(&format!("joining {}", set.name), e));
        }
        let size: u64 = set
            .volumes
            .iter()
            .filter_map(|v| fs::metadata(v).ok())
            .map(|m| m.len())
            .sum();
        if !self.space.has_room(parent, size) {
            return Err(Failure::Infrastructure(format!(
                "not enough free space to unpack {} ({size} bytes)",
                set.name
            )));
        }
        let mut candidates: Vec<Option<&str>> =
            self.passwords.iter().map(|p| Some(p.as_str())).collect();
        if candidates.is_empty() {
            candidates.push(None);
        }
        let mut refused = String::new();
        for &password in &candidates {
            let into = scan::extracting(parent);
            fs::create_dir(&into)
                .map_err(|e| Failure::io(&format!("creating {}", into.display()), e))?;
            let attempt = tool::extract(
                self.unrar,
                self.sevenzip,
                set.family,
                &set.volumes[0],
                &into,
                password,
                self.stall,
            )
            .await;
            let landed = match attempt {
                Attempt::Extracted => Some(tree::adopt(&into, parent)),
                Attempt::WrongPassword(reason) => {
                    refused = reason;
                    None
                }
                Attempt::Failed(failure) => Some(Err(failure)),
            };
            let _ = fs::remove_dir_all(&into);
            if let Some(landed) = landed {
                return landed;
            }
        }
        let given = candidates.last().copied().flatten();
        Err(Failure::Content(crate::jobs::password_failure(
            given, &refused,
        )))
    }
}

fn key(dir: &Path, first: &Path) -> String {
    first
        .strip_prefix(dir)
        .unwrap_or(first)
        .to_string_lossy()
        .into_owned()
}

fn remove(volumes: &[PathBuf]) -> Result<(), Failure> {
    for path in volumes.iter().skip(1).chain(volumes.first()) {
        match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(Failure::io(&format!("removing {}", path.display()), e));
            }
            _ => {}
        }
    }
    Ok(())
}
