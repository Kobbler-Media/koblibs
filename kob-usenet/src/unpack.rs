mod adopt;
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
        adopt_orphans(dir).await?;
        for _ in 0..DEPTH {
            let sets = scan::sets(dir)
                .map_err(|e| Failure::io(&format!("listing {}", dir.display()), e))?;
            if sets.is_empty() {
                break;
            }
            for set in sets {
                let key = {
                    let (root, family, volumes) =
                        (dir.to_path_buf(), set.family, set.volumes.clone());
                    off_runtime(&set.name, move || scan::identity(&root, family, &volumes)).await?
                };
                let parent = set.volumes[0].parent().unwrap_or(dir);
                let landing = scan::landing(parent, &key);
                if !extracted.contains(&key) {
                    if !landing.exists() {
                        self.unpack(&set, &landing).await?;
                    }
                    extracted.push(key);
                    record(&extracted)?;
                }
                remove(&set.volumes)?;
                if landing.is_dir() {
                    adopting(landing, parent.to_path_buf()).await?;
                }
            }
        }
        Ok(())
    }

    async fn unpack(&self, set: &Set, landing: &Path) -> Result<(), Failure> {
        let parent = set.volumes[0].parent().unwrap_or_else(|| Path::new("."));
        if set.family == Family::Split {
            let into = scan::extracting(parent);
            fs::create_dir(&into)
                .map_err(|e| Failure::io(&format!("creating {}", into.display()), e))?;
            let joined = tree::join(&into, &set.name, &set.volumes)
                .map_err(|e| Failure::io(&format!("joining {}", set.name), e))
                .and_then(|()| seal(&into, landing));
            let _ = fs::remove_dir_all(&into);
            return joined;
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
        let mut maybe_damaged = false;
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
                Attempt::Extracted => Some(seal(&into, landing)),
                Attempt::WrongPassword(reason) => {
                    refused = reason;
                    None
                }
                Attempt::DamagedOrWrongPassword(reason) => {
                    refused = reason;
                    maybe_damaged = true;
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
        if maybe_damaged && given.is_some() {
            return Err(Failure::Content(format!(
                "{refused} failed its checksum: the archive is damaged or the password is wrong"
            )));
        }
        Err(Failure::Content(crate::jobs::password_failure(
            given, &refused,
        )))
    }
}

// A landing whose archive is gone was cut off between deleting the volumes and moving in.
async fn adopt_orphans(dir: &Path) -> Result<(), Failure> {
    let root = dir.to_path_buf();
    let (landed, live) = off_runtime("the unpacked files", move || {
        let landed = scan::landed(&root)?;
        if landed.is_empty() {
            return Ok((landed, Vec::new()));
        }
        let live = scan::sets(&root)?
            .iter()
            .map(|set| scan::identity(&root, set.family, &set.volumes))
            .collect::<std::io::Result<Vec<_>>>()?;
        Ok((landed, live))
    })
    .await?;
    for (landing, key) in landed {
        if !live.contains(&key) {
            let parent = landing.parent().unwrap_or(dir).to_path_buf();
            adopting(landing, parent).await?;
        }
    }
    Ok(())
}

async fn adopting(landing: PathBuf, parent: PathBuf) -> Result<(), Failure> {
    tokio::task::spawn_blocking(move || adopt::adopt(&landing, &parent))
        .await
        .map_err(|e| Failure::io("moving unpacked files", e))?
}

async fn off_runtime<T: Send + 'static>(
    what: &str,
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Failure::io(&format!("reading {what}"), e))?
        .map_err(|e| Failure::io(&format!("reading {what}"), e))
}

fn seal(into: &Path, landing: &Path) -> Result<(), Failure> {
    tree::confined(into)?;
    tree::unreserve(into)
        .map_err(|e| Failure::io(&format!("naming files in {}", into.display()), e))?;
    fs::rename(into, landing)
        .map_err(|e| Failure::io(&format!("moving to {}", landing.display()), e))
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::space::Statvfs;

    fn archive(dir: &Path, name: &str, member: &str) -> PathBuf {
        std::fs::write(dir.join(member), member.repeat(100)).unwrap();
        let made = Command::new("7z")
            .args(["a", "-bd", name, member])
            .current_dir(dir)
            .output();
        std::fs::remove_file(dir.join(member)).unwrap();
        assert!(made.is_ok_and(|o| o.status.success()), "7z is required");
        dir.join(name)
    }

    async fn run(dir: &Path, extracted: Vec<String>, fail_records: usize) -> Result<(), Failure> {
        let gate = SpaceGate::new(Arc::new(Statvfs), 0);
        let unpack = Unpack {
            unrar: Path::new("/nonexistent/unrar"),
            sevenzip: Path::new("7z"),
            passwords: &[],
            space: &gate,
            stall: Duration::from_mins(1),
        };
        let records = AtomicUsize::new(0);
        let record = |_: &[String]| {
            if records.fetch_add(1, Ordering::SeqCst) < fail_records {
                return Err(Failure::Infrastructure("crash".to_owned()));
            }
            Ok(())
        };
        unpack.run(dir, extracted, &record).await
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut all: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        all.sort();
        all
    }

    #[tokio::test]
    async fn a_crash_before_the_key_is_recorded_neither_loses_nor_reextracts() {
        let dir = tempfile::tempdir().unwrap();
        archive(dir.path(), "a.7z", "movie.bin");
        assert!(run(dir.path(), Vec::new(), 1).await.is_err());
        assert!(
            dir.path().join("a.7z").exists(),
            "volumes outlive the crash"
        );
        run(dir.path(), Vec::new(), 0).await.ok().unwrap();
        assert_eq!(names(dir.path()), ["movie.bin"]);
    }

    #[tokio::test]
    async fn a_crash_between_deleting_volumes_and_moving_in_keeps_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let landing = scan::landing(dir.path(), "feed");
        std::fs::create_dir(&landing).unwrap();
        std::fs::write(landing.join("movie.bin"), b"data").unwrap();
        run(dir.path(), vec!["feed".to_owned()], 0)
            .await
            .ok()
            .unwrap();
        assert_eq!(names(dir.path()), ["movie.bin"]);
    }

    #[tokio::test]
    async fn a_crash_before_moving_in_keeps_a_colliding_sibling_after_resume() {
        let dir = tempfile::tempdir().unwrap();
        let landing = scan::landing(dir.path(), "feed");
        std::fs::create_dir(&landing).unwrap();
        std::fs::write(landing.join("movie.bin"), b"incoming").unwrap();
        std::fs::write(dir.path().join("movie.bin"), b"resident").unwrap();
        run(dir.path(), vec!["feed".to_owned()], 0)
            .await
            .ok()
            .unwrap();
        assert_eq!(names(dir.path()), ["movie.bin", "movie.bin.1"]);
        assert_eq!(fs::read(dir.path().join("movie.bin")).unwrap(), b"resident");
        assert_eq!(
            fs::read(dir.path().join("movie.bin.1")).unwrap(),
            b"incoming"
        );
    }

    #[tokio::test]
    async fn split_sets_differing_only_mid_volume_are_both_joined() {
        let dir = tempfile::tempdir().unwrap();
        let part = |fill: u8| {
            let mut data = vec![0u8; 3 << 20];
            data[3 << 19] = fill;
            data
        };
        for (stem, fill) in [("m", 1), ("n", 2)] {
            fs::write(dir.path().join(format!("{stem}.mkv.001")), part(fill)).unwrap();
            fs::write(dir.path().join(format!("{stem}.mkv.002")), b"tail").unwrap();
        }
        run(dir.path(), Vec::new(), 0).await.ok().unwrap();
        assert_eq!(names(dir.path()), ["m.mkv", "n.mkv"]);
        assert_eq!(fs::read(dir.path().join("n.mkv")).unwrap()[3 << 19], 2);
    }

    #[tokio::test]
    async fn the_same_archive_in_two_folders_unpacks_in_both() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one"), dir.path().join("two"));
        fs::create_dir(&one).unwrap();
        fs::create_dir(&two).unwrap();
        let made = archive(&one, "a.7z", "movie.bin");
        fs::copy(&made, two.join("a.7z")).unwrap();
        run(dir.path(), Vec::new(), 0).await.ok().unwrap();
        assert_eq!(names(&one), ["movie.bin"]);
        assert_eq!(names(&two), ["movie.bin"]);
    }

    #[tokio::test]
    async fn a_key_already_recorded_is_not_extracted_again() {
        let dir = tempfile::tempdir().unwrap();
        let first = archive(dir.path(), "a.7z", "movie.bin");
        let key = scan::identity(dir.path(), Family::SevenZip, &[first]).unwrap();
        run(dir.path(), vec![key], 0).await.ok().unwrap();
        assert!(names(dir.path()).is_empty());
    }
}
