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
    article: usize,
}

impl Posted {
    pub fn new(prefix: &str, name: &str, data: Vec<u8>) -> Self {
        Self::sized(prefix, name, data, ARTICLE)
    }

    pub fn sized(prefix: &str, name: &str, data: Vec<u8>, article: usize) -> Self {
        let total = data.len().div_ceil(article);
        let articles = (0..total)
            .map(|part| {
                let range = part * article..((part + 1) * article).min(data.len());
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
            article,
        }
    }

    pub fn uneven(prefix: &str, name: &str, data: Vec<u8>, sizes: &[usize]) -> Self {
        assert_eq!(sizes.iter().sum::<usize>(), data.len());
        let mut at = 0;
        let articles = sizes
            .iter()
            .enumerate()
            .map(|(part, &len)| {
                let header = Header {
                    name,
                    part: part + 1,
                    total: sizes.len(),
                    size: data.len(),
                    range: (at, at + len),
                };
                let payload = &data[at..at + len];
                at += len;
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
            article: sizes[0],
        }
    }

    pub fn claiming(prefix: &str, name: &str, data: Vec<u8>, claimed: usize) -> Self {
        let mut posted = Self::new(prefix, name, data);
        for part in 0..posted.articles.len() {
            let header = Header {
                size: claimed,
                ..posted.header(part)
            };
            let payload = &posted.data[posted.range(part)];
            posted.articles[part].wire = wire(header, payload, crc32(0, payload));
        }
        posted
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
        part * self.article..((part + 1) * self.article).min(self.data.len())
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
