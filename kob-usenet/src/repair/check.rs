use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use md5::{Digest, Md5};

use crate::repair::spool::MAX_SLICE;
use crate::vendor::core::Par2Proof;
use crate::vendor::par2::{Id, Slice};
use crate::vendor::yenc::crc32;

const CHUNK: usize = 1 << 20;
static ZEROS: [u8; 4096] = [0; 4096];

pub fn proof_damage(path: &Path, proof: &Par2Proof) -> io::Result<Vec<usize>> {
    let slices: Vec<Slice> = proof
        .slice_md5
        .iter()
        .zip(&proof.slice_crc32)
        .map(|(&md5, &crc32)| Slice { md5, crc32 })
        .collect();
    damaged_slices(
        path,
        proof.file_length,
        u64::from(proof.slice_size),
        &slices,
        &proof.file_md5,
        &|| false,
    )
}

pub fn damaged_slices(
    path: &Path,
    length: u64,
    slice_size: u64,
    slices: &[Slice],
    hash: &Id,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Vec<usize>> {
    let everything = || (0..slices.len().max(1)).collect();
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(everything()),
        Err(e) => return Err(e),
    };
    if file.metadata()?.len() != length {
        return Ok(everything());
    }
    // A slice size past the cap is hostile: only the whole-file hash can be trusted to run.
    let sliced = !slices.is_empty() && slice_size > 0 && slice_size <= MAX_SLICE;
    let step = if sliced { slice_size } else { length.max(1) };
    let mut whole = Md5::new();
    let mut bad = Vec::new();
    let mut buf = vec![0; CHUNK];
    let mut left = length;
    let mut index = 0;
    while left > 0 {
        let span = left.min(step);
        let (mut part, mut piece, mut crc) = (span, Md5::new(), 0);
        while part > 0 {
            if cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let n = usize::try_from(part).map_or(CHUNK, |p| p.min(CHUNK));
            file.read_exact(&mut buf[..n])?;
            whole.update(&buf[..n]);
            if sliced {
                piece.update(&buf[..n]);
                crc = crc32(crc, &buf[..n]);
            }
            part -= n as u64;
        }
        if sliced {
            let mut pad = step - span;
            while pad > 0 {
                let n = usize::try_from(pad).map_or(ZEROS.len(), |p| p.min(ZEROS.len()));
                piece.update(&ZEROS[..n]);
                crc = crc32(crc, &ZEROS[..n]);
                pad -= n as u64;
            }
            let actual = Slice {
                md5: piece.finalize().into(),
                crc32: crc,
            };
            if slices
                .get(index)
                .is_some_and(|expected| *expected != actual)
            {
                bad.push(index);
            }
        }
        left -= span;
        index += 1;
    }
    if bad.is_empty() && <Id>::from(whole.finalize()) != *hash {
        return Ok(everything());
    }
    Ok(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slice_size_past_the_cap_allocates_nothing_and_trusts_only_the_whole_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"content").unwrap();
        let right: Id = Md5::digest(b"content").into();
        let slices = [Slice {
            md5: [1; 16],
            crc32: 2,
        }];
        for slice_size in [1 << 60, 1 << 40, MAX_SLICE + 4] {
            let check = |hash| damaged_slices(&path, 7, slice_size, &slices, hash, &|| false);
            assert!(check(&right).unwrap().is_empty());
            assert_eq!(check(&[0; 16]).unwrap(), [0]);
        }
    }

    #[test]
    fn a_short_last_slice_is_checked_with_its_zero_padding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let data = vec![5u8; 10];
        std::fs::write(&path, &data).unwrap();
        let slices = [
            Slice::checksum(8, &data[..8]).unwrap(),
            Slice::checksum(8, &data[8..]).unwrap(),
        ];
        let hash: Id = Md5::digest(&data).into();
        assert!(
            damaged_slices(&path, 10, 8, &slices, &hash, &|| false)
                .unwrap()
                .is_empty()
        );
        let wrong = [
            slices[0],
            Slice {
                crc32: 0,
                ..slices[1]
            },
        ];
        assert_eq!(
            damaged_slices(&path, 10, 8, &wrong, &hash, &|| false).unwrap(),
            [1]
        );
    }
}
