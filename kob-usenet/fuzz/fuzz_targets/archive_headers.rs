#![no_main]

use kob_usenet::vendor::archive::rar::{self, RAR4_SIGNATURE, RAR5_SIGNATURE};
use kob_usenet::vendor::archive::sevenzip;
use libfuzzer_sys::fuzz_target;

const MAX_STEPS: usize = 4096;

fn window(volume: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok().filter(|&s| s < volume.len())?;
    let end = usize::try_from(offset.saturating_add(len))
        .unwrap_or(usize::MAX)
        .min(volume.len());
    Some(&volume[start..end])
}

fn scan_rar(volume: &[u8]) {
    let mut scanner = rar::Scanner::new();
    let (mut at, mut buf, mut last) = (0, &[][..], None);
    for _ in 0..MAX_STEPS {
        match scanner.next(at, buf) {
            Ok(rar::Step::Need { offset, len }) => {
                if last == Some((offset, len)) {
                    return;
                }
                last = Some((offset, len));
                let Some(bytes) = window(volume, offset, len) else {
                    return;
                };
                (at, buf) = (offset, bytes);
            }
            Ok(rar::Step::Entry(entry)) => assert!(entry.data.start <= entry.data.end),
            Ok(rar::Step::Key(_) | rar::Step::End { .. }) | Err(_) => return,
        }
    }
}

fn read_sevenzip(archive: &[u8]) {
    let mut reader = sevenzip::Reader::new();
    let (mut at, mut buf, mut last) = (0, &[][..], None);
    for _ in 0..MAX_STEPS {
        match reader.next(at, buf) {
            Ok(sevenzip::Step::Need { offset, len }) => {
                if last == Some((offset, len)) {
                    return;
                }
                last = Some((offset, len));
                let Some(bytes) = window(archive, offset, len) else {
                    return;
                };
                (at, buf) = (offset, bytes);
            }
            Ok(sevenzip::Step::Done(entries)) => {
                for entry in entries {
                    assert!(entry.data.is_none_or(|r| r.start <= r.end));
                }
                return;
            }
            Ok(sevenzip::Step::Key(_)) | Err(_) => return,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    scan_rar(data);
    scan_rar(&[RAR5_SIGNATURE, data].concat());
    scan_rar(&[RAR4_SIGNATURE, data].concat());
    read_sevenzip(data);
});
