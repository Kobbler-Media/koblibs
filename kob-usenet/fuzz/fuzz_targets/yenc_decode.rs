#![no_main]

use std::net::{Ipv4Addr, TcpListener as StdListener};
use std::sync::LazyLock;
use std::time::Duration;

use kob_usenet::vendor::nntp::{BodyExpectation, Connection, Endpoint, TlsMode};
use kob_usenet::vendor::yenc::{DecodeEnd, DecoderState, decode, decode_incremental};
use libfuzzer_sys::fuzz_target;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
});

static LISTENER: LazyLock<StdListener> = LazyLock::new(|| {
    let listener = StdListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
});

fn chunked_decode_matches_whole(src: &[u8], split: usize) {
    for raw in [false, true] {
        let mut whole = vec![0; src.len()];
        let n = decode(raw, src, &mut whole, &mut DecoderState::default());
        let (a, b) = src.split_at(split.min(src.len()));
        let mut parts = vec![0; src.len()];
        let mut state = DecoderState::default();
        let m = decode(raw, a, &mut parts, &mut state);
        let m = m + decode(raw, b, &mut parts[m..], &mut state);
        assert_eq!(whole[..n], parts[..m]);
    }
}

fn incremental_terminates(src: &[u8], step: usize) {
    let mut out = Vec::with_capacity(src.len());
    let mut state = DecoderState::default();
    let mut pos = 0;
    while pos < src.len() {
        let chunk = &src[pos..(pos + step).min(src.len())];
        let (used, end) = decode_incremental(chunk, &mut out, &mut state);
        assert!(used <= chunk.len() && out.len() <= src.len());
        if end != DecodeEnd::None {
            return;
        }
        if used == 0 {
            return;
        }
        pos += used;
    }
}

async fn article(reply: &[u8], expected: BodyExpectation) {
    let listener = TcpListener::from_std(LISTENER.try_clone().unwrap()).unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint = Endpoint::new(
        Ipv4Addr::LOCALHOST.to_string(),
        port,
        TlsMode::Plain,
        None,
        Duration::from_secs(2),
    )
    .unwrap();
    let wire = [
        b"200 ready\r\n222 0 <a@fuzz> body follows\r\n".as_slice(),
        reply,
        b"\r\n.\r\n",
    ]
    .concat();
    let server = async {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let _ = socket.write_all(&wire).await;
        let _ = socket.shutdown().await;
        let _ = socket.read_to_end(&mut Vec::new()).await;
    };
    let client = async {
        let mut connection = Connection::connect(&endpoint).await.ok()?;
        connection.send_body("a@fuzz").ok()?;
        connection.flush().await.ok()?;
        let body = connection.read_body_with(None, expected, || {}).await;
        drop(connection);
        body.ok()
    };
    let ((), body) = tokio::join!(server, client);
    if let Some(body) = body {
        assert!(body.data.len() <= reply.len());
        if let Some(offset) = expected.offset {
            assert_eq!(body.offset, offset);
        }
    }
}

fn expectation(control: u8, value: u8) -> BodyExpectation {
    let wanted = |bit: u8| control & bit != 0;
    BodyExpectation {
        part: wanted(0x01).then_some(u32::from(value % 8)),
        total: wanted(0x02).then_some(u32::from(value / 8 % 8)),
        file_size: wanted(0x04).then_some(u64::from(value) * 64),
        offset: wanted(0x08).then_some(u64::from(value) * 128),
        decoded_len: wanted(0x10).then_some(u64::from(value) * 16),
        ..BodyExpectation::NONE
    }
}

fuzz_target!(|data: &[u8]| {
    let [control, value, src @ ..] = data else {
        return;
    };
    chunked_decode_matches_whole(src, usize::from(*value) * 7);
    incremental_terminates(src, usize::from(value % 61) + 1);
    RUNTIME.block_on(article(src, expectation(*control, *value)));
});
