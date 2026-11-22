use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::vendor::archive::{self, Format};
use crate::vendor::queue::names::{self, Kind};

const TAIL: usize = 400;
const DEPTH: usize = 3;

pub struct Tools<'a> {
    pub unrar: &'a Path,
    pub sevenzip: &'a Path,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    Rar,
    SevenZip,
    Split,
}

type Sets = BTreeMap<(Family, String), Vec<(u32, PathBuf)>>;

pub async fn unpack(tools: &Tools<'_>, dir: &Path, password: Option<&str>) -> Result<(), String> {
    for _ in 0..DEPTH {
        let sets = sets(dir).map_err(|e| format!("listing {}: {e}", dir.display()))?;
        if sets.is_empty() {
            break;
        }
        unpack_sets(tools, dir, password, sets).await?;
    }
    Ok(())
}

async fn unpack_sets(
    tools: &Tools<'_>,
    dir: &Path,
    password: Option<&str>,
    sets: Sets,
) -> Result<(), String> {
    for ((family, set), mut volumes) in sets {
        volumes.sort();
        let paths: Vec<PathBuf> = volumes.into_iter().map(|(_, p)| p).collect();
        match family {
            Family::Split => join(dir, &set, &paths).map_err(|e| format!("joining {set}: {e}"))?,
            Family::Rar => extract(tools, true, &paths[0], dir, password).await?,
            Family::SevenZip => extract(tools, false, &paths[0], dir, password).await?,
        }
        for path in &paths {
            fs::remove_file(path).map_err(|e| format!("removing {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

fn sets(dir: &Path) -> io::Result<Sets> {
    let mut sets = Sets::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        let (family, set, volume) = match names::classify(&name) {
            Kind::Rar { set, volume } => (Family::Rar, set, volume),
            Kind::SevenZip { set, volume } => (Family::SevenZip, set, volume),
            Kind::Split { set, volume } => (Family::Split, set, volume),
            Kind::Par2 => continue,
            Kind::Plain => match sniff(&path)? {
                Some(Format::Rar4 | Format::Rar5) => (Family::Rar, name, 0),
                Some(Format::SevenZip) => (Family::SevenZip, name, 0),
                None => continue,
            },
        };
        sets.entry((family, set)).or_default().push((volume, path));
    }
    Ok(sets)
}

fn sniff(path: &Path) -> io::Result<Option<Format>> {
    let mut head = Vec::with_capacity(8);
    File::open(path)?.take(8).read_to_end(&mut head)?;
    Ok(archive::detect(&head))
}

fn join(dir: &Path, set: &str, parts: &[PathBuf]) -> io::Result<()> {
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

async fn extract(
    tools: &Tools<'_>,
    rar: bool,
    archive: &Path,
    dir: &Path,
    password: Option<&str>,
) -> Result<(), String> {
    let mut tried = Vec::new();
    if rar {
        let mut target = dir.as_os_str().to_owned();
        target.push("/");
        let secret = password.map_or_else(|| "-p-".to_owned(), |pw| format!("-p{pw}"));
        let args = [
            OsString::from("x"),
            "-o+".into(),
            "-y".into(),
            secret.into(),
            archive.into(),
            target,
        ];
        match run(tools.unrar, &args).await {
            Err(e) if e.kind() == io::ErrorKind::NotFound => tried.push(tools.unrar),
            result => return judge(tools.unrar, archive, password, result),
        }
    }
    let mut out = OsString::from("-o");
    out.push(dir);
    let secret = format!("-p{}", password.unwrap_or_default());
    let args = [
        OsString::from("x"),
        "-y".into(),
        secret.into(),
        out,
        archive.into(),
    ];
    match run(tools.sevenzip, &args).await {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            tried.push(tools.sevenzip);
            Err(format!(
                "cannot unpack {}: {} not found",
                archive.display(),
                tried
                    .iter()
                    .map(|t| t.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" and ")
            ))
        }
        result => judge(tools.sevenzip, archive, password, result),
    }
}

async fn run(program: &Path, args: &[OsString]) -> io::Result<std::process::Output> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
}

fn judge(
    program: &Path,
    archive: &Path,
    password: Option<&str>,
    result: io::Result<std::process::Output>,
) -> Result<(), String> {
    let output = result.map_err(|e| format!("running {}: {e}", program.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let said = [output.stdout, output.stderr]
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .join("\n");
    let lower = said.to_lowercase();
    let name = archive.file_name().map_or_else(
        || archive.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    if lower.contains("password") || lower.contains("encrypted") {
        return Err(crate::jobs::password_failure(password, &name));
    }
    let tail: String = said
        .trim()
        .chars()
        .rev()
        .take(TAIL)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Err(format!(
        "{} could not unpack {name} ({}): {tail}",
        program.display(),
        output.status
    ))
}
