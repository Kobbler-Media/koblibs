use crate::jobs::fetch::Fetcher;
use crate::vendor::queue::import::{Inspected, Inspector, Level, Output, Rules};
use crate::vendor::queue::nzb::Nzb;

const EXECUTABLE: [&str; 12] = [
    "exe", "msi", "bat", "cmd", "scr", "com", "lnk", "pif", "ps1", "vbs", "jar", "apk",
];
// srr, srs and url are never imported either, so they cannot be what a release is for.
const SIDECAR: [&str; 13] = [
    "par2", "nfo", "sfv", "txt", "jpg", "jpeg", "png", "gif", "bmp", "webp", "srr", "srs", "url",
];
const NAMED: usize = 3;

#[derive(PartialEq, Eq)]
enum Role {
    Executable,
    Sidecar,
    Payload,
}

fn role(path: &str) -> Role {
    let leaf = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let Some((_, ext)) = leaf.rsplit_once('.') else {
        return Role::Payload;
    };
    let ext = ext.to_ascii_lowercase();
    if EXECUTABLE.contains(&ext.as_str()) {
        Role::Executable
    } else if SIDECAR.contains(&ext.as_str()) {
        Role::Sidecar
    } else {
        Role::Payload
    }
}

pub fn proven(outputs: &[Output]) -> bool {
    outputs
        .iter()
        .any(|o| o.meta.size() > 0 && role(&o.path.join("/")) == Role::Payload)
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
    let mut names: Vec<&str> = report
        .files
        .iter()
        .filter(|f| counted(f))
        .map(|f| f.raw_path.as_str())
        .chain(report.blocked.iter().map(String::as_str))
        .collect();
    if names.iter().any(|n| role(n) == Role::Payload) {
        return None;
    }
    names.retain(|n| role(n) == Role::Executable);
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return None;
    }
    let shown = names[..names.len().min(NAMED)].join(", ");
    Some(format!(
        "Refused before downloading: the release holds only executable or installer files ({shown})."
    ))
}
