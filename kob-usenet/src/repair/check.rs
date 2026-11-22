use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use md5::{Digest, Md5};

use crate::vendor::core::Par2Proof;
use crate::vendor::par2::{Id, Slice};

const CHUNK: usize = 1 << 20;

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
    )
}

pub fn damaged_slices(
    path: &Path,
    length: u64,
    slice_size: u64,
    slices: &[Slice],
    hash: &Id,
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
    let step = if slices.is_empty() {
        CHUNK
    } else {
        usize::try_from(slice_size).unwrap_or(CHUNK)
    };
    let mut whole = Md5::new();
    let mut bad = Vec::new();
    let mut buf = vec![0; step];
    let mut left = length;
    let mut index = 0;
    while left > 0 {
        let n = usize::try_from(left.min(step as u64)).unwrap_or(step);
        file.read_exact(&mut buf[..n])?;
        whole.update(&buf[..n]);
        if let Some(expected) = slices.get(index)
            && !expected.matches(slice_size, &buf[..n])
        {
            bad.push(index);
        }
        left -= n as u64;
        index += 1;
    }
    if bad.is_empty() && <Id>::from(whole.finalize()) != *hash {
        return Ok(everything());
    }
    Ok(bad)
}
