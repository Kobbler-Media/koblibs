use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};

use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, articles_without, assert_file, completed, par2_set, run_job};

const SLICE: usize = ARTICLE / 2 / 4 * 4;
const MAIN: &[u8; 16] = b"PAR 2.0\0Main\0\0\0\0";
const DESC: &[u8; 16] = b"PAR 2.0\0FileDesc";
const IFSC: &[u8; 16] = b"PAR 2.0\0IFSC\0\0\0\0";
const FLOOD: usize = 4 << 20;

fn packet(set: [u8; 16], kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
    let rest = [set.as_slice(), kind, body].concat();
    [
        b"PAR2\0PKT".as_slice(),
        &((32 + rest.len()) as u64).to_le_bytes(),
        &Md5::digest(&rest),
        &rest,
    ]
    .concat()
}

fn huge_slice_set(name: &str, length: u64, slice: u64) -> Vec<u8> {
    let id = [0x42; 16];
    let main = [slice.to_le_bytes().as_slice(), &1u32.to_le_bytes(), &id].concat();
    let mut padded = name.as_bytes().to_vec();
    padded.resize(padded.len().next_multiple_of(4), 0);
    let desc = [
        id.as_slice(),
        &[1; 16],
        &[2; 16],
        &length.to_le_bytes(),
        &padded,
    ]
    .concat();
    let ifsc = [id.as_slice(), &[3; 16], &[4; 4]].concat();
    [
        packet([0; 16], MAIN, &main),
        packet([0; 16], DESC, &desc),
        packet([0; 16], IFSC, &ifsc),
    ]
    .concat()
}

#[tokio::test]
async fn a_set_declaring_an_absurd_slice_size_is_ignored() {
    for slice in [1u64 << 60, 1 << 40, 8 << 30] {
        let ep = Posted::new("ep", "Show.mkv", video(8 * ARTICLE + 100, 9));
        let sets = par2_set(&[&ep], "S", SLICE, 100);
        let mut index = sets[0].data.clone();
        index.extend(huge_slice_set(&ep.name, ep.data.len() as u64, slice));
        let index = Posted::new("idx", &sets[0].name, index);
        let mut posts = vec![&ep, &index];
        posts.extend(&sets[1..]);
        let gone = ep.id(2).to_owned();
        let rig = Rig::new(articles_without(&posts, &[&gone])).await;
        let engine = rig.engine().await;
        let dir = completed(&run_job(&rig, &engine, "Huge", &posts).await);
        assert_file(&dir, &ep.name, &ep.data);
    }
}

fn fake_headers(bytes: usize, claimed: u64, set: [u8; 16]) -> Vec<u8> {
    let mut flood = Vec::with_capacity(bytes + 64);
    while flood.len() < bytes {
        flood.extend_from_slice(b"PAR2\0PKT");
        flood.extend_from_slice(&claimed.to_le_bytes());
        flood.extend_from_slice(&[0x5a; 16]);
        flood.extend_from_slice(&set);
        flood.extend_from_slice(b"PAR 2.0\0RecvSlic");
    }
    flood
}

#[tokio::test]
async fn fake_packet_headers_neither_stall_the_runtime_nor_block_the_repair() {
    for known_set in [true, false] {
        let ep = Posted::new("ep", "Show.mkv", video(8 * ARTICLE + 100, 19));
        let sets = par2_set(&[&ep], "S", SLICE, 100);
        let set: [u8; 16] = if known_set {
            sets[0].data[32..48].try_into().unwrap()
        } else {
            [0x11; 16]
        };
        let flooded: Vec<Posted> = sets
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let flood = fake_headers(FLOOD, FLOOD as u64 / 2, set);
                Posted::new(&format!("f{i}"), &p.name, [p.data.clone(), flood].concat())
            })
            .collect();
        let mut posts = vec![&ep];
        posts.extend(&flooded);
        let gone = ep.id(2).to_owned();
        let rig = Rig::new(articles_without(&posts, &[&gone])).await;
        let engine = rig.engine().await;

        let worst = Arc::new(AtomicU64::new(0));
        let beat = {
            let worst = Arc::clone(&worst);
            tokio::spawn(async move {
                let mut last = Instant::now();
                loop {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    let gap = u64::try_from(last.elapsed().as_millis()).unwrap_or(u64::MAX);
                    worst.fetch_max(gap, Ordering::Relaxed);
                    last = Instant::now();
                }
            })
        };
        let dir = completed(&run_job(&rig, &engine, "Flood", &posts).await);
        beat.abort();
        assert_file(&dir, &ep.name, &ep.data);
        let worst = worst.load(Ordering::Relaxed);
        assert!(worst < 1500, "the runtime stalled for {worst} ms");
    }
}
