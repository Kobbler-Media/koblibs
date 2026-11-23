use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::jobs::failure::Failure;
use crate::store::write_atomic;
use crate::unpack::scan;
use crate::vendor::queue::names::{self, Kind};

const COMPARE: usize = 1 << 20;

type Move = (PathBuf, PathBuf);
type GroupKey = (Option<(u8, String)>, OsString);
type Entry = (OsString, OsString);

// The plan is journaled because a volume set must land under one number: re-planning after a
// crash would see its own half-moved volumes as a collision and split the set.
pub fn adopt(landing: &Path, into: &Path) -> Result<(), Failure> {
    let failed = |e| Failure::io(&format!("moving into {}", into.display()), e);
    let journal = journal(landing);
    let moves = match fs::read(&journal) {
        Ok(saved) => decode(&saved).map_err(failed)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut moves = Vec::new();
            plan(landing, into, Path::new(""), Path::new(""), &mut moves).map_err(failed)?;
            save(&journal, &moves).map_err(failed)?;
            moves
        }
        Err(e) => return Err(failed(e)),
    };
    for (from, to) in &moves {
        apply(&landing.join(from), &into.join(to)).map_err(failed)?;
    }
    fs::remove_dir_all(journal.parent().unwrap_or(landing)).map_err(failed)?;
    fs::remove_dir_all(landing).map_err(failed)
}

// A folder of its own: a crash can strand the atomic write's temp file, and hidden scratch
// folders never reach the finished download.
fn journal(landing: &Path) -> PathBuf {
    let mut name = landing.as_os_str().to_owned();
    name.push(scan::JOURNAL);
    PathBuf::from(name).join("moves")
}

fn save(journal: &Path, moves: &[Move]) -> io::Result<()> {
    fs::create_dir_all(journal.parent().unwrap_or(journal))?;
    write_atomic(journal, &encode(moves)?)
}

fn encode(moves: &[Move]) -> io::Result<Vec<u8>> {
    let raw: Vec<(&[u8], &[u8])> = moves
        .iter()
        .map(|(from, to)| (from.as_os_str().as_bytes(), to.as_os_str().as_bytes()))
        .collect();
    postcard::to_stdvec(&raw).map_err(io::Error::other)
}

fn decode(saved: &[u8]) -> io::Result<Vec<Move>> {
    let raw: Vec<(Vec<u8>, Vec<u8>)> = postcard::from_bytes(saved).map_err(io::Error::other)?;
    let path = |bytes| PathBuf::from(OsString::from_vec(bytes));
    Ok(raw
        .into_iter()
        .map(|(from, to)| (path(from), path(to)))
        .collect())
}

fn apply(from: &Path, to: &Path) -> io::Result<()> {
    match fs::symlink_metadata(from) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    match fs::symlink_metadata(to) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::rename(from, to),
        Err(e) => Err(e),
        Ok(_) if identical(from, to)? => fs::remove_file(from),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("refusing to overwrite {}", to.display()),
        )),
    }
}

fn plan(
    from: &Path,
    into: &Path,
    from_rel: &Path,
    into_rel: &Path,
    moves: &mut Vec<Move>,
) -> io::Result<()> {
    let present = fs::read_dir(into)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<io::Result<HashSet<OsString>>>()?;
    let mut planned = HashSet::new();
    let mut groups: BTreeMap<GroupKey, Vec<Entry>> = BTreeMap::new();
    let mut entries = fs::read_dir(from)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let source = entry.file_name();
        let name = scan::unreserved(&source);
        if !entry.file_type()?.is_dir() {
            let key = match set_of(&name) {
                Some(set) => (Some(set), OsString::new()),
                None => (None, name.clone()),
            };
            groups.entry(key).or_default().push((source, name));
            continue;
        }
        let resident = into.join(&name);
        if fs::symlink_metadata(&resident).is_ok_and(|m| m.is_dir()) {
            plan(
                &from.join(&source),
                &resident,
                &from_rel.join(&source),
                &into_rel.join(&name),
                moves,
            )?;
            continue;
        }
        let target = (0..)
            .map(|n| numbered(&name, n))
            .find(|t| !present.contains(t) && !planned.contains(t))
            .expect("some number is free");
        moves.push((from_rel.join(&source), into_rel.join(&target)));
        planned.insert(target);
    }
    for group in groups.into_values() {
        let targets = (0..)
            .map(|n| {
                group
                    .iter()
                    .map(|(_, name)| named(name, n))
                    .collect::<Vec<_>>()
            })
            .find_map(|targets| {
                fits(&group, &targets, from, into, &present, &planned)
                    .map(|fit| fit.then_some(targets))
                    .transpose()
            })
            .expect("some number is free")?;
        for ((source, _), target) in group.iter().zip(targets) {
            moves.push((from_rel.join(source), into_rel.join(&target)));
            planned.insert(target);
        }
    }
    Ok(())
}

fn fits(
    group: &[Entry],
    targets: &[OsString],
    from: &Path,
    into: &Path,
    present: &HashSet<OsString>,
    planned: &HashSet<OsString>,
) -> io::Result<bool> {
    let set = set_of(&targets[0]);
    if targets.iter().any(|t| set_of(t) != set) || (set.is_some() != set_of(&group[0].1).is_some())
    {
        return Ok(false);
    }
    if set.is_some()
        && present
            .iter()
            .chain(planned)
            .any(|name| set_of(name) == set && !targets.contains(name))
    {
        return Ok(false);
    }
    for ((source, _), target) in group.iter().zip(targets) {
        if planned.contains(target) {
            return Ok(false);
        }
        if present.contains(target) && !identical(&from.join(source), &into.join(target))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn set_of(name: &OsStr) -> Option<(u8, String)> {
    match names::classify(name.to_str()?) {
        Kind::Rar { set, .. } => Some((0, set)),
        Kind::SevenZip { set, .. } => Some((1, set)),
        Kind::Split { set, .. } => Some((2, set)),
        _ => None,
    }
}

// `b.7z.1` would classify as volume 1 of `b.7z`, so an archive's number goes in its set stem.
fn named(name: &OsStr, n: u32) -> OsString {
    if n == 0 {
        return name.to_owned();
    }
    let stem_end = name.to_str().and_then(|text| match names::classify(text) {
        Kind::Rar { set, .. } => Some(set.len()),
        Kind::SevenZip { set, .. } => Some(set.len() - ".7z".len()),
        Kind::Split { set, .. } => set.rfind('.'),
        _ => None,
    });
    match (stem_end, name.to_str()) {
        (Some(end), Some(text)) => format!("{}.{n}{}", &text[..end], &text[end..]).into(),
        _ => numbered(name, n),
    }
}

fn numbered(name: &OsStr, n: u32) -> OsString {
    if n == 0 {
        return name.to_owned();
    }
    let mut out = name.to_owned();
    out.push(format!(".{n}"));
    out
}

fn identical(a: &Path, b: &Path) -> io::Result<bool> {
    let (ma, mb) = (fs::symlink_metadata(a)?, fs::symlink_metadata(b)?);
    if !ma.is_file() || !mb.is_file() || ma.len() != mb.len() {
        return Ok(false);
    }
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    let (mut left, mut right) = (vec![0; COMPARE], vec![0; COMPARE]);
    let mut remaining = ma.len();
    while remaining > 0 {
        let n = usize::try_from(remaining).map_or(COMPARE, |r| r.min(COMPARE));
        a.read_exact(&mut left[..n])?;
        b.read_exact(&mut right[..n])?;
        if left[..n] != right[..n] {
            return Ok(false);
        }
        remaining -= n as u64;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(dir: &Path) -> Vec<String> {
        let mut all: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        all.sort();
        all
    }

    fn rig(incoming: &[(&str, &str)], resident: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let landing = dir.path().join(".unpacked-k");
        fs::create_dir(&landing).unwrap();
        for (name, data) in incoming {
            fs::write(landing.join(name), data).unwrap();
        }
        for (name, data) in resident {
            fs::write(dir.path().join(name), data).unwrap();
        }
        (dir, landing)
    }

    #[test]
    fn renamed_names_stay_in_their_family() {
        for (name, renamed) in [
            ("b.7z", "b.1.7z"),
            ("b.7z.001", "b.1.7z.001"),
            ("x.rar", "x.1.rar"),
            ("x.part02.rar", "x.1.part02.rar"),
            ("x.r00", "x.1.r00"),
            ("movie.mkv.001", "movie.1.mkv.001"),
            ("movie.bin", "movie.bin.1"),
        ] {
            let got = named(OsStr::new(name), 1);
            assert_eq!(got, renamed);
            assert_eq!(
                set_of(&got).map(|s| s.0),
                set_of(OsStr::new(name)).map(|s| s.0),
                "{name}"
            );
        }
    }

    #[test]
    fn a_volume_set_with_one_colliding_part_moves_under_one_number() {
        let (dir, landing) = rig(
            &[("x.part01.rar", "new1"), ("x.part02.rar", "new2")],
            &[("x.part01.rar", "old1")],
        );
        adopt(&landing, dir.path()).ok().unwrap();
        assert_eq!(
            listing(dir.path()),
            ["x.1.part01.rar", "x.1.part02.rar", "x.part01.rar"]
        );
        assert_eq!(fs::read(dir.path().join("x.part01.rar")).unwrap(), b"old1");
    }

    #[test]
    fn a_crash_midway_resumes_the_journaled_names() {
        let (dir, landing) = rig(
            &[("x.part01.rar", "new1"), ("x.part02.rar", "new2")],
            &[("x.part01.rar", "old1")],
        );
        let mut moves = Vec::new();
        plan(
            &landing,
            dir.path(),
            Path::new(""),
            Path::new(""),
            &mut moves,
        )
        .unwrap();
        save(&journal(&landing), &moves).unwrap();
        apply(&landing.join(&moves[0].0), &dir.path().join(&moves[0].1)).unwrap();

        adopt(&landing, dir.path()).ok().unwrap();
        assert_eq!(
            listing(dir.path()),
            ["x.1.part01.rar", "x.1.part02.rar", "x.part01.rar"]
        );
    }

    #[test]
    fn a_file_and_a_folder_of_one_name_both_survive() {
        let (dir, landing) = rig(&[("thing", "file")], &[]);
        fs::create_dir(dir.path().join("thing")).unwrap();
        fs::write(dir.path().join("thing/kept"), b"kept").unwrap();
        adopt(&landing, dir.path()).ok().unwrap();
        assert_eq!(fs::read(dir.path().join("thing/kept")).unwrap(), b"kept");
        assert_eq!(fs::read(dir.path().join("thing.1")).unwrap(), b"file");

        let (dir, landing) = rig(&[], &[("thing", "file")]);
        fs::create_dir(landing.join("thing")).unwrap();
        fs::write(landing.join("thing/new"), b"new").unwrap();
        adopt(&landing, dir.path()).ok().unwrap();
        assert_eq!(fs::read(dir.path().join("thing")).unwrap(), b"file");
        assert_eq!(fs::read(dir.path().join("thing.1/new")).unwrap(), b"new");
    }

    #[test]
    fn a_member_named_like_engine_scratch_survives_a_later_sweep() {
        let (dir, landing) = rig(&[], &[]);
        for name in [".extract-held", ".unpacked-held"] {
            fs::create_dir(landing.join(name)).unwrap();
            fs::write(landing.join(name).join("kept"), name).unwrap();
        }
        scan::sweep(dir.path()).unwrap();
        assert!(landing.join(".extract-held/kept").exists());
        adopt(&landing, dir.path()).ok().unwrap();
        scan::sweep(dir.path()).unwrap();
        assert!(scan::landed(dir.path()).unwrap().is_empty());
        assert_eq!(listing(dir.path()), ["extract-held", "unpacked-held"]);
    }

    #[test]
    fn equal_length_files_differing_late_are_not_deduplicated() {
        let mut late = vec![0u8; COMPARE + 10];
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, &late).unwrap();
        fs::write(&b, &late).unwrap();
        assert!(identical(&a, &b).unwrap());
        late[COMPARE + 5] = 1;
        fs::write(&b, &late).unwrap();
        assert!(!identical(&a, &b).unwrap());
    }
}
