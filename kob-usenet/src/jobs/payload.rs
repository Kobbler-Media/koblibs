use crate::jobs::fetch::Fetcher;
use crate::vendor::queue::import::{Inspected, Inspector, Level, Output, Rules};
use crate::vendor::queue::nzb::Nzb;

const EXECUTABLE: [&str; 11] = [
    "exe", "msi", "bat", "cmd", "scr", "lnk", "pif", "ps1", "vbs", "jar", "apk",
];
const READABLE: [&str; 7] = ["txt", "jpg", "jpeg", "png", "gif", "bmp", "webp"];
// srr, srs and url are never imported either, so they cannot be what a release is for.
const SIDECAR: [&str; 6] = ["par2", "nfo", "sfv", "srr", "srs", "url"];
// A real .com program is tiny, and a release's junk file is often a site's name.
const LARGEST_COM: u64 = 1 << 20;
const NAMED: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Executable,
    Readable,
    Sidecar,
    Payload,
}

fn role(path: &str, size: Option<u64>) -> Role {
    let leaf = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let Some((_, ext)) = leaf.rsplit_once('.') else {
        return Role::Payload;
    };
    let ext = ext.to_ascii_lowercase();
    if ext == "com" {
        let site = leaf.to_ascii_lowercase().contains("www.");
        return if !site && size.is_none_or(|size| size <= LARGEST_COM) {
            Role::Executable
        } else {
            Role::Sidecar
        };
    }
    if EXECUTABLE.contains(&ext.as_str()) {
        Role::Executable
    } else if READABLE.contains(&ext.as_str()) {
        Role::Readable
    } else if SIDECAR.contains(&ext.as_str()) {
        Role::Sidecar
    } else {
        Role::Payload
    }
}

pub fn proven(outputs: &[Output]) -> bool {
    outputs
        .iter()
        .any(|o| o.meta.size() > 0 && role(&o.path.join("/"), Some(o.meta.size())) == Role::Payload)
}

// Images and text are the release when they are at least twice its programs (a scanned book
// with a bundled viewer); a program of unknown size could be any size, so it never is.
fn refused<'a>(files: &[(&'a str, Option<u64>)]) -> Option<Vec<&'a str>> {
    let of = |wanted: Role| {
        files
            .iter()
            .filter(move |&&(name, size)| role(name, size) == wanted)
    };
    if of(Role::Payload).next().is_some() {
        return None;
    }
    let readable: u64 = of(Role::Readable).filter_map(|&(_, size)| size).sum();
    let programs: Option<u64> = of(Role::Executable).map(|&(_, size)| size).sum();
    if programs.is_some_and(|programs| readable >= programs.saturating_mul(2)) {
        return None;
    }
    let mut names: Vec<&str> = of(Role::Executable).map(|&(name, _)| name).collect();
    names.sort_unstable();
    names.dedup();
    (!names.is_empty()).then_some(names)
}

fn counted(file: &Inspected) -> bool {
    file.size != Some(0)
}

pub async fn only_executables(
    fetcher: &Fetcher,
    nzb: &Nzb,
    release: &str,
    rules: &Rules,
    password: Option<&str>,
) -> Option<String> {
    let Level::Report(report) = Inspector::default()
        .inspect(&fetcher.client, nzb, release, rules, password)
        .await
    else {
        return None;
    };
    if !report.manifest_complete && !report.only_blocked {
        return None;
    }
    let files: Vec<(&str, Option<u64>)> = report
        .files
        .iter()
        .filter(|f| counted(f))
        .map(|f| (f.raw_path.as_str(), f.size))
        .chain(report.blocked.iter().map(|b| (b.name.as_str(), b.size)))
        .collect();
    let names = refused(&files)?;
    let shown = names[..names.len().min(NAMED)].join(", ");
    Some(format!(
        "Refused before downloading: the release holds only executable or installer files ({shown})."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;

    #[test]
    fn what_a_release_holds_decides_whether_it_is_refused() {
        type Case<'a> = (&'a str, &'a [(&'a str, Option<u64>)], bool);
        let cases: &[Case<'_>] = &[
            (
                "exe release",
                &[("Movie.2026.exe", Some(GB)), ("Movie.nfo", Some(900))],
                true,
            ),
            (
                "exe with every small sidecar",
                &[
                    ("Movie.2026.exe", Some(GB)),
                    ("Movie.nfo", Some(900)),
                    ("Movie.sfv", Some(90)),
                    ("Movie.par2", Some(40_000)),
                    ("cover.jpg", Some(200_000)),
                    ("readme.txt", Some(3_000)),
                ],
                true,
            ),
            (
                "msi with art",
                &[("Setup.msi", Some(800 * MB)), ("art.png", Some(5 * MB))],
                true,
            ),
            (
                "small real com",
                &[("Crack.com", Some(40_000)), ("info.nfo", Some(900))],
                true,
            ),
            (
                "com of unknown size",
                &[("Crack.com", None), ("info.nfo", Some(900))],
                true,
            ),
            (
                "exe of unknown size beside images",
                &[("a.exe", None), ("b.jpg", Some(GB))],
                true,
            ),
            (
                "image book with a viewer",
                &[
                    ("Artbook/001.jpg", Some(60 * MB)),
                    ("Artbook/002.png", Some(60 * MB)),
                    ("Artbook/viewer.exe", Some(2 * MB)),
                ],
                false,
            ),
            (
                "novel beside a junk site file",
                &[("Novel.txt", Some(MB)), ("www.example.com", Some(120))],
                false,
            ),
            (
                "scans beside a spaced junk site name",
                &[
                    ("Scans/p001.jpeg", Some(4 * MB)),
                    ("Visit www.example.com", Some(80)),
                ],
                false,
            ),
            (
                "a large file called .com",
                &[("Show.S01E01.com", Some(GB))],
                false,
            ),
            (
                "movie with a stray exe",
                &[("Movie.mkv", Some(GB)), ("setup.exe", Some(MB))],
                false,
            ),
            (
                "book with a stray exe",
                &[("Book.epub", Some(MB)), ("x.exe", Some(MB))],
                false,
            ),
            ("only images", &[("scan-001.jpg", Some(MB))], false),
            (
                "images not clearly outweighing the program",
                &[
                    ("a.exe", Some(45 * MB)),
                    ("b.jpg", Some(40 * MB)),
                    ("c.txt", Some(15 * MB)),
                ],
                true,
            ),
        ];
        for (what, files, refuse) in cases {
            assert_eq!(refused(files).is_some(), *refuse, "{what}");
        }
    }

    #[test]
    fn a_large_or_site_named_com_does_not_prove_a_payload_either() {
        for (name, size) in [("www.example.com", 120), ("Show.S01E01.com", GB)] {
            assert!(role(name, Some(size)) == Role::Sidecar, "{name}");
        }
    }
}
