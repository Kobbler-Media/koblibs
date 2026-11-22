use kob_usenet::vendor::par2::{Slice, hash_16k};
use md5::{Digest, Md5};

const SLICE: u64 = 4096;

pub fn index(files: &[(&str, &[u8])]) -> Vec<u8> {
    let described: Vec<([u8; 16], Vec<u8>, Vec<u8>)> = files
        .iter()
        .map(|&(name, data)| {
            let start = hash_16k(data);
            let id: [u8; 16] = Md5::digest(
                [
                    start.as_slice(),
                    &(data.len() as u64).to_le_bytes(),
                    name.as_bytes(),
                ]
                .concat(),
            )
            .into();
            let desc = [
                id.as_slice(),
                &Md5::digest(data),
                &start,
                &(data.len() as u64).to_le_bytes(),
                name.as_bytes(),
            ]
            .concat();
            let checksums = data
                .chunks(SLICE as usize)
                .flat_map(|slice| {
                    let s = Slice::checksum(SLICE, slice).unwrap();
                    [s.md5.as_slice(), &s.crc32.to_le_bytes()].concat()
                })
                .collect::<Vec<u8>>();
            (id, desc, [id.as_slice(), &checksums].concat())
        })
        .collect();
    let mut ids: Vec<[u8; 16]> = described.iter().map(|(id, _, _)| *id).collect();
    ids.sort_unstable();
    let main = [
        SLICE.to_le_bytes().as_slice(),
        &u32::try_from(ids.len()).unwrap().to_le_bytes(),
        &ids.concat(),
    ]
    .concat();
    let set: [u8; 16] = Md5::digest(&main).into();
    let mut out = packet(&set, b"PAR 2.0\0Main\0\0\0\0", &main);
    for (_, desc, ifsc) in &described {
        out.extend(packet(&set, b"PAR 2.0\0FileDesc", desc));
        out.extend(packet(&set, b"PAR 2.0\0IFSC\0\0\0\0", ifsc));
    }
    out
}

fn packet(set: &[u8; 16], kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
    let mut rest = [set.as_slice(), kind, body].concat();
    rest.resize(rest.len().next_multiple_of(4), 0);
    let len = 32 + rest.len() as u64;
    [
        b"PAR2\0PKT".as_slice(),
        &len.to_le_bytes(),
        &Md5::digest(&rest),
        &rest,
    ]
    .concat()
}
