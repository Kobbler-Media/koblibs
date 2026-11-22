use std::ops::Range;

use kob_usenet::vendor::testkit::Article;
use kob_usenet::vendor::yenc::{crc32, encode};

pub const ARTICLE: usize = 16 * 1024;
const EBML: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(len);
    out
}

pub fn video(len: usize, seed: u64) -> Vec<u8> {
    let mut data = noise(len, seed);
    data[..EBML.len()].copy_from_slice(&EBML);
    data
}

#[derive(Clone, Copy)]
pub struct Header<'a> {
    pub name: &'a str,
    pub part: usize,
    pub total: usize,
    pub size: usize,
    pub range: (usize, usize),
}

pub fn wire(header: Header<'_>, payload: &[u8], crc: u32) -> Vec<u8> {
    let Header {
        name,
        part,
        total,
        size,
        range: (begin, end),
    } = header;
    let mut out = format!(
        "=ybegin part={part} total={total} line=128 size={size} name={name}\r\n=ypart begin={} end={end}\r\n",
        begin + 1
    )
    .into_bytes();
    let mut line_start = true;
    for &byte in &encode(payload, 128) {
        if line_start && byte == b'.' {
            out.push(b'.');
        }
        out.push(byte);
        line_start = byte == b'\n';
    }
    out.extend_from_slice(
        format!(
            "\r\n=yend size={} part={part} pcrc32={crc:08x}\r\n.\r\n",
            payload.len()
        )
        .as_bytes(),
    );
    out
}

#[derive(Clone)]
pub struct Posted {
    pub name: String,
    pub data: Vec<u8>,
    pub articles: Vec<Article>,
}

impl Posted {
    pub fn new(prefix: &str, name: &str, data: Vec<u8>) -> Self {
        let total = data.len().div_ceil(ARTICLE);
        let articles = (0..total)
            .map(|part| {
                let range = part * ARTICLE..((part + 1) * ARTICLE).min(data.len());
                let header = Header {
                    name,
                    part: part + 1,
                    total,
                    size: data.len(),
                    range: (range.start, range.end),
                };
                let payload = &data[range];
                Article {
                    message_id: format!("{prefix}-{part}@loopback"),
                    wire: wire(header, payload, crc32(0, payload)),
                }
            })
            .collect();
        Self {
            name: name.to_owned(),
            data,
            articles,
        }
    }

    pub fn header(&self, part: usize) -> Header<'_> {
        let range = self.range(part);
        Header {
            name: &self.name,
            part: part + 1,
            total: self.articles.len(),
            size: self.data.len(),
            range: (range.start, range.end),
        }
    }

    pub fn range(&self, part: usize) -> Range<usize> {
        part * ARTICLE..((part + 1) * ARTICLE).min(self.data.len())
    }

    pub fn id(&self, part: usize) -> &str {
        &self.articles[part].message_id
    }
}

pub fn articles<'a>(posts: impl IntoIterator<Item = &'a Posted>) -> Vec<Article> {
    posts
        .into_iter()
        .flat_map(|p| p.articles.iter().cloned())
        .collect()
}
