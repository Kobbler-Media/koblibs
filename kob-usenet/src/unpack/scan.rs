use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::vendor::archive::rar::{Scanner, Step};
use crate::vendor::archive::{self, Format};
use crate::vendor::queue::names::{self, Kind};

const ZIP: &[u8] = b"PK\x03\x04";
const HEADER_READ: u64 = 4 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    Rar,
    SevenZip,
    Zip,
    Split,
}

#[derive(Debug)]
pub struct Set {
    pub family: Family,
    pub name: String,
    pub volumes: Vec<PathBuf>,
}

#[derive(Debug, Default)]
struct RarVolume {
    multi: bool,
    first: bool,
    number: Option<u32>,
    opens: Option<String>,
    closes: Option<String>,
}

pub fn sets(dir: &Path) -> io::Result<Vec<Set>> {
    let mut files = Vec::new();
    walk(dir, &mut files)?;
    let mut named: BTreeMap<(PathBuf, Family, String), Vec<(u32, PathBuf)>> = BTreeMap::new();
    let mut headed: BTreeMap<PathBuf, Vec<(PathBuf, RarVolume)>> = BTreeMap::new();
    for path in files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        let parent = path.parent().unwrap_or(dir).to_path_buf();
        let (family, set, volume) = match names::classify(&name) {
            Kind::Rar { set, volume } => (Family::Rar, set, volume),
            Kind::SevenZip { set, volume } => (Family::SevenZip, set, volume),
            Kind::Split { set, volume } => (Family::Split, set, volume),
            Kind::Par2 => continue,
            Kind::Plain => match sniff(&path)? {
                Some(Sniffed::Rar) => {
                    let header = rar_volume(&path)?;
                    if header.multi {
                        headed.entry(parent).or_default().push((path, header));
                        continue;
                    }
                    (Family::Rar, name, 0)
                }
                Some(Sniffed::SevenZip) => (Family::SevenZip, name, 0),
                Some(Sniffed::Zip) => (Family::Zip, name, 0),
                None => continue,
            },
        };
        named
            .entry((parent, family, set))
            .or_default()
            .push((volume, path));
    }
    let mut sets: Vec<Set> = named
        .into_iter()
        .map(|((_, family, name), mut volumes)| {
            volumes.sort();
            Set {
                family,
                name,
                volumes: volumes.into_iter().map(|(_, p)| p).collect(),
            }
        })
        .collect();
    for (parent, volumes) in headed {
        sets.extend(chain(&parent, volumes)?);
    }
    Ok(sets)
}

const EXTRACTING: &str = ".extract-";

pub fn sweep(dir: &Path) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if entry.file_name().to_string_lossy().starts_with(EXTRACTING) {
            fs::remove_dir_all(entry.path())?;
        } else {
            sweep(&entry.path())?;
        }
    }
    Ok(())
}

pub fn extracting(parent: &Path) -> PathBuf {
    parent.join(format!("{EXTRACTING}{}", uuid::Uuid::new_v4()))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        let hidden = entry.file_name().to_string_lossy().starts_with(EXTRACTING);
        if kind.is_dir() && !hidden {
            walk(&path, out)?;
        } else if kind.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

enum Sniffed {
    Rar,
    SevenZip,
    Zip,
}

fn sniff(path: &Path) -> io::Result<Option<Sniffed>> {
    let mut head = Vec::with_capacity(8);
    File::open(path)?.take(8).read_to_end(&mut head)?;
    Ok(match archive::detect(&head) {
        Some(Format::Rar4 | Format::Rar5) => Some(Sniffed::Rar),
        Some(Format::SevenZip) => Some(Sniffed::SevenZip),
        None if head.starts_with(ZIP) => Some(Sniffed::Zip),
        None => None,
    })
}

fn rar_volume(path: &Path) -> io::Result<RarVolume> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut scanner = Scanner::new();
    let mut found = RarVolume::default();
    let (mut at, mut buf) = (0, Vec::new());
    loop {
        match scanner.next(at, &buf) {
            Ok(Step::Need { offset, .. }) if offset == at && !buf.is_empty() => break,
            Ok(Step::Need { offset, len: want }) if offset < len => {
                let end = (offset + want.min(HEADER_READ)).min(len);
                buf = vec![0; usize::try_from(end - offset).unwrap_or(0)];
                file.read_exact_at(&mut buf, offset)?;
                at = offset;
            }
            Ok(Step::Entry(entry)) => {
                if found.opens.is_none() && entry.split_before {
                    found.opens = Some(entry.name.clone());
                }
                found.closes = entry.split_after.then_some(entry.name);
            }
            Ok(_) | Err(_) => break,
        }
    }
    let volume = scanner.volume();
    found.multi = volume.multi;
    found.first = volume.first;
    found.number = volume.number;
    Ok(found)
}

fn chain(parent: &Path, mut volumes: Vec<(PathBuf, RarVolume)>) -> io::Result<Vec<Set>> {
    volumes.sort_by(|a, b| a.0.cmp(&b.0));
    let mut taken = vec![false; volumes.len()];
    let mut sets = Vec::new();
    let firsts: Vec<usize> = (0..volumes.len())
        .filter(|&i| volumes[i].1.first || volumes[i].1.number == Some(0))
        .collect();
    for (n, &first) in firsts.iter().enumerate() {
        taken[first] = true;
        let mut order = vec![first];
        loop {
            let last = &volumes[*order.last().expect("starts with the first")].1;
            let number = last.number.map(|v| v + 1);
            let next = |i: usize| {
                let v = &volumes[i].1;
                !taken[i]
                    && !v.first
                    && (number.is_none() || v.number.is_none() || v.number == number)
            };
            let by_member = (0..volumes.len())
                .find(|&i| next(i) && last.closes.is_some() && volumes[i].1.opens == last.closes);
            let only = || {
                let mut candidates = (0..volumes.len()).filter(|&i| next(i));
                let one = candidates.next()?;
                candidates.next().is_none().then_some(one)
            };
            let Some(i) = by_member.or_else(only) else {
                break;
            };
            taken[i] = true;
            order.push(i);
        }
        let name = format!(".obfuscated-{n}");
        let mut paths = Vec::new();
        for (k, &i) in order.iter().enumerate() {
            let renamed = parent.join(format!("{name}.part{:03}.rar", k + 1));
            fs::rename(&volumes[i].0, &renamed)?;
            paths.push(renamed);
        }
        sets.push(Set {
            family: Family::Rar,
            name,
            volumes: paths,
        });
    }
    for (i, (path, _)) in volumes.into_iter().enumerate() {
        if !taken[i] {
            sets.push(Set {
                family: Family::Rar,
                name: path.to_string_lossy().into_owned(),
                volumes: vec![path],
            });
        }
    }
    Ok(sets)
}
