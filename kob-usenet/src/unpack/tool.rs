use std::ffi::OsString;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use crate::jobs::failure::Failure;
use crate::unpack::scan::Family;
use crate::unpack::tree;

const TAIL: usize = 400;
const KEEP: usize = 64 << 10;
const TICK: Duration = Duration::from_millis(100);
const WRONG_PASSWORD: [&str; 5] = [
    "incorrect password",
    "the specified password is incorrect",
    "error: wrong password",
    "can not open encrypted archive",
    "cannot open encrypted archive",
];
// RAR4 and 7-Zip cannot tell a wrong key from damaged data: both fail the checksum.
const DAMAGED_OR_WRONG: [&str; 2] = ["in the encrypted file", "in encrypted file"];
const NO_SPACE: [&str; 3] = ["no space left", "disk full", "not enough space"];
const UNRAR_MACHINE: [i32; 3] = [5, 8, 9];
const SEVENZIP_MACHINE: [i32; 1] = [8];
const PROGRESS: [&str; 5] = ["extracting", "creating", "inflating", "- ", "path ="];

pub enum Attempt {
    Extracted,
    WrongPassword(String),
    DamagedOrWrongPassword(String),
    Failed(Failure),
}

pub async fn extract(
    unrar: &Path,
    sevenzip: &Path,
    family: Family,
    archive: &Path,
    into: &Path,
    password: Option<&str>,
    stall: Duration,
) -> Attempt {
    let mut missing = Vec::new();
    if family == Family::Rar {
        let mut target = into.as_os_str().to_owned();
        target.push("/");
        let secret = if password.is_some() { "-p" } else { "-p-" };
        let args = [
            OsString::from("x"),
            "-o+".into(),
            "-y".into(),
            secret.into(),
            "--".into(),
            archive.into(),
            target,
        ];
        match run(unrar, &args, password, into, stall).await {
            Err(Run::Spawn(e)) if e.kind() == io::ErrorKind::NotFound => missing.push(unrar),
            result => return judge(unrar, &UNRAR_MACHINE, archive, result),
        }
    }
    let mut out = OsString::from("-o");
    out.push(into);
    let mut args = vec![OsString::from("x"), "-y".into(), out];
    if password.is_none() {
        args.push("-p".into());
    }
    args.extend(["--".into(), archive.into()]);
    match run(sevenzip, &args, password, into, stall).await {
        Err(Run::Spawn(e)) if e.kind() == io::ErrorKind::NotFound => {
            missing.push(sevenzip);
            Attempt::Failed(Failure::Infrastructure(format!(
                "cannot unpack {}: {} not found",
                archive.display(),
                missing
                    .iter()
                    .map(|t| t.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" and ")
            )))
        }
        result => judge(sevenzip, &SEVENZIP_MACHINE, archive, result),
    }
}

enum Run {
    Spawn(io::Error),
    Io(io::Error),
    Stalled(Duration),
}

struct Finished {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run(
    program: &Path,
    args: &[OsString],
    password: Option<&str>,
    into: &Path,
    stall: Duration,
) -> Result<Finished, Run> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if password.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(Run::Spawn)?;
    if let (Some(mut stdin), Some(password)) = (child.stdin.take(), password) {
        let line = format!("{password}\n");
        let _ = stdin.write_all(line.as_bytes()).await;
    }
    watch(&mut child, into, stall).await
}

async fn watch(child: &mut Child, into: &Path, stall: Duration) -> Result<Finished, Run> {
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let (mut out_buf, mut err_buf) = ([0u8; 4096], [0u8; 4096]);
    let (mut out_open, mut err_open) = (true, true);
    // Apart: a tool's progress on one stream can land inside an error on the other.
    let (mut said_out, mut said_err) = (Vec::new(), Vec::new());
    let mut last = Instant::now();
    let mut size = tree::size(into);
    loop {
        tokio::select! {
            read = stdout.read(&mut out_buf), if out_open => match read.map_err(Run::Io)? {
                0 => out_open = false,
                n => { keep(&mut said_out, &out_buf[..n]); last = Instant::now(); }
            },
            read = stderr.read(&mut err_buf), if err_open => match read.map_err(Run::Io)? {
                0 => err_open = false,
                n => { keep(&mut said_err, &err_buf[..n]); last = Instant::now(); }
            },
            status = child.wait(), if !out_open && !err_open => {
                return Ok(Finished {
                    status: status.map_err(Run::Io)?,
                    stdout: said_out,
                    stderr: said_err,
                });
            }
            () = tokio::time::sleep(TICK) => {
                let now = tree::size(into);
                if now != size {
                    size = now;
                    last = Instant::now();
                }
                if last.elapsed() >= stall {
                    let _ = child.kill().await;
                    return Err(Run::Stalled(stall));
                }
            }
        }
    }
}

fn keep(said: &mut Vec<u8>, data: &[u8]) {
    said.extend_from_slice(data);
    if said.len() > KEEP {
        said.drain(..said.len() - KEEP);
    }
}

fn judge(
    program: &Path,
    machine: &[i32],
    archive: &Path,
    result: Result<Finished, Run>,
) -> Attempt {
    let name = archive.file_name().map_or_else(
        || archive.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let finished = match result {
        Ok(finished) => finished,
        Err(Run::Spawn(e) | Run::Io(e)) => {
            return Attempt::Failed(Failure::io(&format!("running {}", program.display()), e));
        }
        Err(Run::Stalled(after)) => {
            return Attempt::Failed(Failure::Infrastructure(format!(
                "{} stalled unpacking {name}: no output or progress for {after:?}",
                program.display()
            )));
        }
    };
    let stdout = String::from_utf8_lossy(&finished.stdout);
    let stderr = String::from_utf8_lossy(&finished.stderr);
    let lower = [stdout.to_lowercase(), stderr.to_lowercase()];
    let lines = || {
        lower
            .iter()
            .flat_map(|said| said.lines())
            .map(str::trim)
            .filter(|l| !PROGRESS.iter().any(|p| l.starts_with(p)))
    };
    if finished.status.success() {
        return Attempt::Extracted;
    }
    if let Some(signal) = finished.status.signal() {
        return Attempt::Failed(Failure::Infrastructure(format!(
            "{} was killed by signal {signal} unpacking {name}",
            program.display()
        )));
    }
    if let Some(code) = finished.status.code()
        && machine.contains(&code)
    {
        return Attempt::Failed(Failure::Infrastructure(format!(
            "{} hit a write, memory or file creation error unpacking {name} (exit status {code})",
            program.display()
        )));
    }
    if lines().any(|l| NO_SPACE.iter().any(|p| l.contains(p))) {
        return Attempt::Failed(Failure::Infrastructure(format!(
            "{} ran out of disk space unpacking {name}",
            program.display()
        )));
    }
    if lines().any(|l| WRONG_PASSWORD.iter().any(|p| l.contains(p))) {
        return Attempt::WrongPassword(name);
    }
    if lines().any(|l| DAMAGED_OR_WRONG.iter().any(|p| l.contains(p))) {
        return Attempt::DamagedOrWrongPassword(name);
    }
    let said = if stderr.trim().is_empty() {
        &stdout
    } else {
        &stderr
    };
    let tail: String = said
        .trim()
        .chars()
        .rev()
        .take(TAIL)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Attempt::Failed(Failure::Content(format!(
        "{} could not unpack {name} ({}): {tail}",
        program.display(),
        finished.status
    )))
}
