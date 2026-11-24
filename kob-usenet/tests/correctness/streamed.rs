use std::io::{self, SeekFrom};
use std::time::Duration;

use kob_usenet::vendor::core::trace::{Outcome, Reason};
use kob_usenet::vendor::testkit::Article;
use kob_usenet::vendor::yenc::crc32;
use kob_usenet::{Engine, OutputReader, Purpose, ReleaseKey};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::time::timeout;

use crate::harness::{Rig, output};
use crate::nzb::nzb;
use crate::post::{Header, Posted, noise, video, wire};

const BIG: usize = 4 << 20;
const SMALL: usize = 700 * 1024;
const TARGET: usize = 1;
const HEAD: usize = 256 * 1024;
const PROMPT: Duration = Duration::from_secs(3);
const QUIET: Duration = Duration::from_millis(500);

struct Film {
    post: Posted,
    rig: Rig,
    key: ReleaseKey,
    index: usize,
}

impl Film {
    async fn new(article: usize, providers: impl FnOnce(&Posted) -> Vec<Vec<Article>>) -> Self {
        let post = Posted::sized(
            "film",
            "Film.2026.mkv",
            video(4 * article - 1000, 7),
            article,
        );
        let rig = Rig::new(providers(&post)).await;
        let release = rig.import(&nzb(&[post.listed()])).await.unwrap();
        let index = output(&release, &post.name).unwrap();
        Self {
            post,
            rig,
            key: release.key,
            index,
        }
    }

    fn start(&self) -> usize {
        self.post.range(TARGET).start
    }

    fn end(&self) -> usize {
        self.post.range(TARGET).end
    }

    async fn open(&self, purpose: Purpose, at: usize) -> OutputReader {
        let mut reader = self
            .rig
            .engine
            .open(&self.key, self.index, purpose)
            .unwrap();
        reader.seek(SeekFrom::Start(at as u64)).await.unwrap();
        reader
    }

    fn hold(&self, provider: usize, after: usize) -> tokio::sync::oneshot::Sender<()> {
        self.rig.servers[provider]
            .hold_article_after(self.post.id(TARGET), after)
            .1
    }

    fn truth(&self, at: usize, len: usize) -> &[u8] {
        &self.post.data[at..at + len]
    }

    async fn served_whole(&self, at: usize) {
        let mut reader = self.open(Purpose::Play, at).await;
        let mut rest = vec![0; self.end() - at];
        timeout(PROMPT, reader.read_exact(&mut rest))
            .await
            .expect("the re-request is served")
            .expect("the re-request gets the article");
        assert!(rest == self.truth(at, rest.len()), "correct bytes");
    }
}

fn with_target(post: &Posted, wire: Vec<u8>) -> Vec<Article> {
    let mut articles = post.articles.clone();
    articles[TARGET].wire = wire;
    articles
}

fn corrupt(engine: &Engine, key: &str) -> u64 {
    let corrupt = Outcome::NotRetrieved(Reason::Corrupt).index();
    engine
        .stats()
        .iter()
        .filter(|p| p.key == key)
        .flat_map(|p| p.workloads.iter())
        .map(|w| w.attempts[corrupt])
        .sum()
}

async fn missed(engine: &Engine, key: &str) {
    for _ in 0..300 {
        if corrupt(engine, key) > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{key} never counted the corrupt copy");
}

async fn read_until_error(reader: &mut OutputReader, got: &mut Vec<u8>) -> io::Result<()> {
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        got.extend_from_slice(&buf[..n]);
    }
}

fn line_after(wire: &[u8], at: usize) -> (usize, usize) {
    let start = at + wire[at..].iter().position(|&b| b == b'\n').unwrap() + 1;
    let end = start + wire[start..].iter().position(|&b| b == b'\r').unwrap();
    (start, end)
}

fn flipped(wire: &[u8], at: usize) -> Vec<u8> {
    let (start, _) = line_after(wire, at);
    let mut wire = wire.to_vec();
    let at = start
        + 1
        + wire[start + 1..]
            .iter()
            .position(|&b| b.is_ascii_lowercase() && b != b'z')
            .unwrap();
    wire[at] += 1;
    wire
}

#[tokio::test]
async fn a_large_article_reaches_playback_before_it_is_whole() {
    let film = Film::new(BIG, |post| vec![post.articles.clone()]).await;
    let wire = film.post.articles[TARGET].wire.len();
    let release = film.hold(0, wire / 2);
    let mut reader = film.open(Purpose::Play, film.start()).await;

    let mut head = vec![0; HEAD];
    timeout(PROMPT, reader.read_exact(&mut head))
        .await
        .expect("the first bytes arrive while the rest of the article is held")
        .unwrap();
    assert!(head == film.truth(film.start(), HEAD));

    release.send(()).unwrap();
    let mut rest = vec![0; film.end() - film.start() - HEAD];
    timeout(PROMPT, reader.read_exact(&mut rest))
        .await
        .unwrap()
        .unwrap();
    assert!(rest == film.truth(film.start() + HEAD, rest.len()));
}

#[tokio::test]
async fn a_seek_into_a_large_article_starts_at_the_offset() {
    let film = Film::new(BIG, |post| vec![post.articles.clone()]).await;
    let wire = film.post.articles[TARGET].wire.len();
    let _release = film.hold(0, wire * 3 / 4);
    let at = film.start() + BIG / 2 + 12_345;
    let mut reader = film.open(Purpose::Play, at).await;

    let mut head = vec![0; 64 * 1024];
    timeout(PROMPT, reader.read_exact(&mut head))
        .await
        .expect("bytes past the offset arrive before the article is whole")
        .unwrap();
    assert!(head == film.truth(at, head.len()));
}

#[tokio::test]
async fn a_small_article_reaches_playback_only_once_verified() {
    let film = Film::new(SMALL, |post| vec![post.articles.clone()]).await;
    let wire = film.post.articles[TARGET].wire.len();
    let release = film.hold(0, wire / 2);
    let mut reader = film.open(Purpose::Play, film.start()).await;

    let mut head = vec![0; 1024];
    assert!(
        timeout(QUIET, reader.read_exact(&mut head)).await.is_err(),
        "nothing is served while the article is incomplete"
    );
    release.send(()).unwrap();
    timeout(PROMPT, reader.read_exact(&mut head))
        .await
        .unwrap()
        .unwrap();
    assert!(head == film.truth(film.start(), head.len()));
}

#[tokio::test]
async fn cache_and_warming_reads_never_see_an_unverified_article() {
    for purpose in [Purpose::Cache, Purpose::Warm] {
        let film = Film::new(BIG, |post| vec![post.articles.clone()]).await;
        let wire = film.post.articles[TARGET].wire.len();
        let release = film.hold(0, wire / 2);
        let mut reader = film.open(purpose, film.start()).await;

        let mut head = vec![0; 1024];
        assert!(
            timeout(QUIET, reader.read_exact(&mut head)).await.is_err(),
            "{purpose:?} is served nothing while the article is incomplete"
        );
        release.send(()).unwrap();
        timeout(PROMPT, reader.read_exact(&mut head))
            .await
            .unwrap()
            .unwrap();
        assert!(head == film.truth(film.start(), head.len()), "{purpose:?}");
    }
}

#[tokio::test]
async fn an_article_that_fails_its_crc_after_streaming_never_reaches_the_cache() {
    let film = Film::new(BIG, |post| {
        let wire = &post.articles[TARGET].wire;
        vec![with_target(post, flipped(wire, wire.len() * 3 / 4))]
    })
    .await;
    let wire = film.post.articles[TARGET].wire.len();
    let release = film.hold(0, wire / 2);
    let mut reader = film.open(Purpose::Play, film.start()).await;

    let mut got = Vec::new();
    let early = timeout(QUIET, read_until_error(&mut reader, &mut got)).await;
    assert!(early.is_err(), "the read waits on the held article");
    assert!(got.len() >= HEAD, "playback streamed {} bytes", got.len());
    release.send(()).unwrap();
    let ended = timeout(PROMPT, read_until_error(&mut reader, &mut got))
        .await
        .unwrap();
    assert!(ended.is_err(), "the read ends in an error");
    assert!(got.len() < BIG, "the article's last byte waits for its CRC");
    drop(reader);
    missed(&film.rig.engine, "p0").await;

    for purpose in [Purpose::Cache, Purpose::Play] {
        let mut reader = film.open(purpose, film.start()).await;
        let read = timeout(PROMPT, reader.read(&mut [0; 1])).await.unwrap();
        assert!(
            read.is_err(),
            "{purpose:?} finds no copy of the failed article"
        );
    }
}

#[tokio::test]
async fn a_crc_failure_ends_playback_in_an_error_and_the_re_request_is_verified() {
    let film = Film::new(BIG, |post| {
        let wire = &post.articles[TARGET].wire;
        vec![
            with_target(post, flipped(wire, wire.len() * 3 / 4)),
            post.articles.clone(),
        ]
    })
    .await;
    let wire = film.post.articles[TARGET].wire.len();
    let release = film.hold(0, wire / 2);
    let mut reader = film.open(Purpose::Play, film.start()).await;

    let mut got = Vec::new();
    let early = timeout(QUIET, read_until_error(&mut reader, &mut got)).await;
    assert!(early.is_err(), "the read waits on the held article");
    assert!(got.len() >= HEAD, "playback streamed {} bytes", got.len());
    assert!(got[..HEAD] == *film.truth(film.start(), HEAD));
    release.send(()).unwrap();
    let ended = timeout(PROMPT, read_until_error(&mut reader, &mut got))
        .await
        .unwrap();
    assert!(ended.is_err(), "the read ends in an error, not a clean end");
    drop(reader);
    missed(&film.rig.engine, "p0").await;

    film.served_whole(film.start() + got.len()).await;
    film.served_whole(film.start()).await;
}

#[tokio::test]
async fn a_wrong_header_serves_nothing_and_fails_over() {
    type Wrong = fn(Header<'_>) -> Header<'_>;
    let cases: [(&str, Wrong); 5] = [
        ("part number", |h| Header {
            part: h.part + 2,
            ..h
        }),
        ("offsets", |h| Header {
            range: (h.range.0 + 128, h.range.1 + 128),
            ..h
        }),
        ("end offset", |h| Header {
            range: (h.range.0, h.range.1 - 128),
            ..h
        }),
        ("file size", |h| Header {
            size: h.size + 1,
            ..h
        }),
        ("file name", |h| Header {
            name: "Other.2026.mkv",
            ..h
        }),
    ];
    for (case, wrong) in cases {
        let film = Film::new(BIG, |post| {
            let header = wrong(post.header(TARGET));
            let payload = noise(header.range.1 - header.range.0, 99);
            let substitute = wire(header, &payload, crc32(0, &payload));
            vec![with_target(post, substitute), post.articles.clone()]
        })
        .await;
        let mut reader = film.open(Purpose::Play, film.start()).await;
        let mut got = vec![0; film.end() - film.start()];
        timeout(PROMPT, reader.read_exact(&mut got))
            .await
            .unwrap_or_else(|_| panic!("{case}: the read finishes"))
            .unwrap_or_else(|e| panic!("{case}: {e}"));
        assert!(
            got == film.truth(film.start(), got.len()),
            "{case}: only the next provider's bytes are served"
        );
        assert!(corrupt(&film.rig.engine, "p0") > 0, "{case}: a miss on p0");
    }
}

#[tokio::test]
async fn a_structural_fault_mid_article_ends_playback_at_once_and_the_re_request_is_verified() {
    type Fault = fn(&mut Vec<u8>, usize) -> usize;
    let cases: [(&str, Fault); 6] = [
        ("raw NUL", |w, at| {
            let (start, _) = line_after(w, at);
            w[start + 10] = 0;
            start + 200
        }),
        ("bare CR", |w, at| {
            let (start, _) = line_after(w, at);
            w[start + 10] = b'\r';
            start + 200
        }),
        ("short line", |w, at| {
            let (_, end) = line_after(w, at);
            let cut = if w[end - 6] == b'=' { 6 } else { 5 };
            w.drain(end - cut..end);
            end + 200
        }),
        ("long line", |w, at| {
            let (_, end) = line_after(w, at);
            w.splice(end..end, *b"aaaaa");
            end + 200
        }),
        ("early =yend", |w, at| {
            let (start, _) = line_after(w, at);
            w.splice(start..start, *b"=yend size=1 part=2 pcrc32=00000000\r\n");
            start + 200
        }),
        ("data past the part's end", |w, _| {
            let end = w.windows(7).rposition(|s| s == b"\r\n=yend").unwrap();
            let extra = [&b"\r\n"[..], &[b'a'; 128], b"\r\n", &[b'a'; 128]].concat();
            w.splice(end..end, extra);
            end + 400
        }),
    ];
    for (case, fault) in cases {
        let mut hold = 0;
        let film = Film::new(BIG, |post| {
            let mut wire = post.articles[TARGET].wire.clone();
            let at = wire.len() * 3 / 4;
            hold = fault(&mut wire, at);
            vec![with_target(post, wire), post.articles.clone()]
        })
        .await;
        let wire = film.post.articles[TARGET].wire.len();
        let first = film.hold(0, wire / 2);
        let release = film.hold(0, hold);
        let mut reader = film.open(Purpose::Play, film.start()).await;

        let mut got = vec![0; HEAD];
        timeout(PROMPT, reader.read_exact(&mut got))
            .await
            .unwrap_or_else(|_| panic!("{case}: the head streams"))
            .unwrap();
        first.send(()).unwrap();
        let ended = timeout(PROMPT, read_until_error(&mut reader, &mut got))
            .await
            .unwrap_or_else(|_| panic!("{case}: the read stops while the rest is held"));
        assert!(ended.is_err(), "{case}: the read ends in an error");
        assert!(
            got.len() >= HEAD,
            "{case}: {} bytes streamed first",
            got.len()
        );
        assert!(
            got == film.truth(film.start(), got.len()),
            "{case}: nothing from the faulty part was served"
        );
        drop(reader);
        release.send(()).unwrap();
        missed(&film.rig.engine, "p0").await;

        film.served_whole(film.start() + got.len()).await;
    }
}
