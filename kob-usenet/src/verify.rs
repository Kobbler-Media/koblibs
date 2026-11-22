use tokio::io::AsyncReadExt;

use crate::Error;
use crate::vendor::queue::import::Error as ImportError;
use crate::vendor::vfs::Reader;

const HEAD: usize = 400;
const TS_PACKET: usize = 188;
const MEDIA: [&str; 12] = [
    "mkv", "webm", "mka", "mp4", "m4v", "mov", "m4a", "avi", "ts", "m2ts", "mpg", "vob",
];

pub async fn decrypted(path: &str, mut reader: Reader, len: u64) -> Result<(), Error> {
    let mut head = vec![0; HEAD.min(usize::try_from(len).unwrap_or(HEAD))];
    reader.read_exact(&mut head).await?;
    if known(&head) {
        return Ok(());
    }
    let extension = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    Err(Error::Import(match extension {
        Some(e) if MEDIA.contains(&e.as_str()) => ImportError::Password(format!(
            "Wrong archive password: {path} does not decrypt to a .{e} file"
        )),
        _ => ImportError::Unsupported(format!(
            "{path} is encrypted and its password cannot be checked while streaming; download the release instead"
        )),
    }))
}

fn known(head: &[u8]) -> bool {
    let at = |offset: usize, magic: &[u8]| head.get(offset..offset + magic.len()) == Some(magic);
    at(0, &[0x1A, 0x45, 0xDF, 0xA3])
        || [b"ftyp", b"moov", b"mdat", b"free", b"wide", b"skip"]
            .iter()
            .any(|m| at(4, *m))
        || at(0, b"RIFF")
        || sync(head, 0, TS_PACKET)
        || sync(head, 4, TS_PACKET + 4)
        || at(0, &[0, 0, 1, 0xBA])
        || at(0, b"Rar!\x1a\x07")
        || at(0, &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C])
        || at(0, b"PK\x03\x04")
}

fn sync(head: &[u8], first: usize, packet: usize) -> bool {
    (0..3).all(|n| head.get(first + n * packet).is_none_or(|&b| b == 0x47))
        && head.get(first) == Some(&0x47)
}
