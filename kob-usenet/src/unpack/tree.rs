use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::jobs::failure::Failure;

pub fn size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => size(&e.path()),
            _ => e.metadata().map_or(0, |m| m.len()),
        })
        .sum()
}

pub fn adopt(from: &Path, into: &Path) -> Result<(), Failure> {
    let root = from
        .canonicalize()
        .map_err(|e| Failure::io(&format!("reading {}", from.display()), e))?;
    check(&root, &root)?;
    merge(&root, into).map_err(|e| Failure::io(&format!("moving into {}", into.display()), e))
}

fn check(root: &Path, dir: &Path) -> Result<(), Failure> {
    let entries =
        fs::read_dir(dir).map_err(|e| Failure::io(&format!("reading {}", dir.display()), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| Failure::io("reading an unpacked entry", e))?;
        let path = entry.path();
        let kind = fs::symlink_metadata(&path)
            .map_err(|e| Failure::io(&format!("reading {}", path.display()), e))?
            .file_type();
        let named = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        if kind.is_symlink() {
            return Err(Failure::Content(format!(
                "the archive holds a symbolic link ({named}); refusing to unpack it"
            )));
        }
        let real = path
            .canonicalize()
            .map_err(|e| Failure::io(&format!("reading {}", path.display()), e))?;
        if !real.starts_with(root) {
            return Err(Failure::Content(format!(
                "the archive writes outside its folder ({named}); refusing to unpack it"
            )));
        }
        if kind.is_dir() {
            check(root, &path)?;
        } else if !kind.is_file() {
            return Err(Failure::Content(format!(
                "the archive holds a special file ({named}); refusing to unpack it"
            )));
        }
    }
    Ok(())
}

fn merge(from: &Path, into: &Path) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target: PathBuf = into.join(entry.file_name());
        let is_dir = entry.file_type()?.is_dir();
        match fs::symlink_metadata(&target) {
            Ok(existing) if is_dir && existing.is_dir() => {
                merge(&entry.path(), &target)?;
                continue;
            }
            Ok(existing) if existing.is_dir() => fs::remove_dir_all(&target)?,
            Ok(_) => fs::remove_file(&target)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        fs::rename(entry.path(), &target)?;
    }
    Ok(())
}

pub fn join(dir: &Path, set: &str, parts: &[PathBuf]) -> io::Result<()> {
    let name = parts[0]
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.rsplit_once('.'))
        .map_or_else(|| set.to_owned(), |(stem, _)| stem.to_owned());
    let mut out = File::create(dir.join(name))?;
    for part in parts {
        io::copy(&mut File::open(part)?, &mut out)?;
    }
    out.flush()
}
