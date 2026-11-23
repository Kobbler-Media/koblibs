use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const TMP: &str = ".tmp-";

pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!("{TMP}{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn sweep_tmp(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if is_tmp(&entry.path()) {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

pub fn touch(path: &Path) {
    if let Ok(file) = fs::File::options().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

pub fn evict(dir: &Path, max_bytes: u64, max_files: usize, keep: Option<&Path>) -> io::Result<()> {
    let mut entries: Vec<(SystemTime, u64, PathBuf)> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|entry| !is_tmp(&entry.path()))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), entry.path()))
        })
        .collect();
    let mut bytes: u64 = entries.iter().map(|(_, len, _)| len).sum();
    let mut files = entries.len();
    entries.sort();
    entries.pop();
    for (_, len, path) in entries {
        if bytes <= max_bytes && files <= max_files {
            break;
        }
        if Some(path.as_path()) != keep && fs::remove_file(&path).is_ok() {
            bytes -= len;
            files -= 1;
        }
    }
    Ok(())
}

fn is_tmp(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(TMP))
}
