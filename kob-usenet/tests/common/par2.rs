use std::sync::LazyLock;

use kob_usenet::vendor::par2::{Slice, hash_16k};
use md5::{Digest, Md5};

pub const SLICE: usize = 4096;

struct Described<'a> {
    id: [u8; 16],
    desc: Vec<u8>,
    ifsc: Vec<u8>,
    data: &'a [u8],
}

pub struct Set {
    slice: usize,
    id: [u8; 16],
    critical: Vec<u8>,
    inputs: Vec<Vec<u8>>,
}

pub fn index(files: &[(&str, &[u8])]) -> Vec<u8> {
    Set::new(files).index()
}

impl Set {
    pub fn new(files: &[(&str, &[u8])]) -> Self {
        Self::sliced(files, SLICE)
    }

    pub fn sliced(files: &[(&str, &[u8])], slice: usize) -> Self {
        let mut described: Vec<Described<'_>> = files
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
                    .chunks(slice)
                    .flat_map(|piece| {
                        let s = Slice::checksum(slice as u64, piece).unwrap();
                        [s.md5.as_slice(), &s.crc32.to_le_bytes()].concat()
                    })
                    .collect::<Vec<u8>>();
                Described {
                    id,
                    desc,
                    ifsc: [id.as_slice(), &checksums].concat(),
                    data,
                }
            })
            .collect();
        described.sort_by_key(|d| d.id);
        let ids: Vec<u8> = described.iter().flat_map(|d| d.id).collect();
        let main = [
            (slice as u64).to_le_bytes().as_slice(),
            &u32::try_from(described.len()).unwrap().to_le_bytes(),
            &ids,
        ]
        .concat();
        let id: [u8; 16] = Md5::digest(&main).into();
        let mut critical = packet(&id, b"PAR 2.0\0Main\0\0\0\0", &main);
        for d in &described {
            critical.extend(packet(&id, b"PAR 2.0\0FileDesc", &d.desc));
            critical.extend(packet(&id, b"PAR 2.0\0IFSC\0\0\0\0", &d.ifsc));
        }
        let inputs = described
            .iter()
            .flat_map(|d| d.data.chunks(slice))
            .map(|piece| {
                let mut padded = piece.to_vec();
                padded.resize(slice, 0);
                padded
            })
            .collect();
        Self {
            slice,
            id,
            critical,
            inputs,
        }
    }

    pub fn index(&self) -> Vec<u8> {
        self.critical.clone()
    }

    pub fn volume(&self, first: u32, count: u32) -> Vec<u8> {
        let mut out: Vec<u8> = (first..first + count)
            .flat_map(|exponent| {
                packet(
                    &self.id,
                    b"PAR 2.0\0RecvSlic",
                    &[exponent.to_le_bytes().as_slice(), &self.recovery(exponent)].concat(),
                )
            })
            .collect();
        out.extend(&self.critical);
        out
    }

    fn recovery(&self, exponent: u32) -> Vec<u8> {
        let mut row = vec![0u16; self.slice / 2];
        for (input, data) in self.inputs.iter().enumerate() {
            let factor = GF.pow(GF.constants[input], exponent);
            for (word, pair) in row.iter_mut().zip(data.chunks(2)) {
                *word ^= GF.mul(factor, u16::from_le_bytes([pair[0], pair[1]]));
            }
        }
        row.iter().flat_map(|w| w.to_le_bytes()).collect()
    }
}

pub fn volume_name(set: &str, first: u32, count: u32) -> String {
    format!("{set}.vol{first:02}+{count:02}.par2")
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

struct Field {
    log: Vec<u32>,
    exp: Vec<u16>,
    constants: Vec<u16>,
}

static GF: LazyLock<Field> = LazyLock::new(|| {
    let mut log = vec![0; 65_536];
    let mut exp = vec![0; 65_535];
    let mut value = 1u32;
    for (index, entry) in exp.iter_mut().enumerate() {
        *entry = value as u16;
        log[value as usize] = index as u32;
        value <<= 1;
        if value & 0x1_0000 != 0 {
            value ^= 0x1_100b;
        }
    }
    let constants = (1..65_535usize)
        .filter(|n| n % 3 != 0 && n % 5 != 0 && n % 17 != 0 && n % 257 != 0)
        .map(|n| exp[n])
        .collect();
    Field {
        log,
        exp,
        constants,
    }
});

impl Field {
    fn mul(&self, a: u16, b: u16) -> u16 {
        if a == 0 || b == 0 {
            return 0;
        }
        self.exp[((self.log[a as usize] + self.log[b as usize]) % 65_535) as usize]
    }

    fn pow(&self, value: u16, exponent: u32) -> u16 {
        let power = u64::from(self.log[value as usize]) * u64::from(exponent);
        self.exp[(power % 65_535) as usize]
    }
}
