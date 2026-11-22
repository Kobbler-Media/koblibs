use std::path::Path;

use kob_usenet::vendor::testkit::{Article, LoopbackServer, post};
use kob_usenet::{Engine, EngineConfig, ImportMode, IpPreference, Release, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub const ARTICLE: usize = 16 * 1024;
const EBML: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];
pub const POSTED: i64 = 1_790_000_000;

pub struct Posted {
    pub name: &'static str,
    pub data: Vec<u8>,
    pub articles: Vec<Article>,
}

pub struct Fixture {
    pub files: Vec<Posted>,
}

impl Fixture {
    pub fn season() -> Self {
        let file = |name: &'static str, prefix: &str, len: usize, seed: u8| {
            let mut data: Vec<u8> = (0..len)
                .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
                .collect();
            if name.ends_with(".mkv") {
                data[..4].copy_from_slice(&EBML);
            }
            let articles = post(prefix, name, &data, ARTICLE);
            Posted {
                name,
                data,
                articles,
            }
        };
        Self {
            files: vec![
                file("Show.S01E02.mkv", "e02", 9 * ARTICLE + 1234, 7),
                file("Show.S01E01.mkv", "e01", 12 * ARTICLE + 777, 3),
                file("Show.S01E01.srt", "sub", 3000, 11),
            ],
        }
    }

    pub fn articles(&self) -> Vec<Article> {
        self.files
            .iter()
            .flat_map(|f| f.articles.iter().cloned())
            .collect()
    }

    pub fn without(&self, message_id: &str) -> Vec<Article> {
        self.articles()
            .into_iter()
            .filter(|a| a.message_id != message_id)
            .collect()
    }

    pub fn data(&self, name: &str) -> &[u8] {
        &self
            .files
            .iter()
            .find(|f| f.name == name)
            .expect("posted file")
            .data
    }

    pub fn nzb(
        &self,
        order: &[usize],
        subject: fn(&str, usize, usize) -> String,
        date: i64,
    ) -> Vec<u8> {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n",
        );
        for (n, &i) in order.iter().enumerate() {
            let file = &self.files[i];
            xml.push_str(&format!(
                "<file poster=\"p\" date=\"{date}\" subject=\"{}\"><groups><group>a.b.test</group></groups><segments>\n",
                subject(file.name, n + 1, order.len()).replace('"', "&quot;")
            ));
            for (part, article) in file.articles.iter().enumerate() {
                xml.push_str(&format!(
                    "<segment bytes=\"{}\" number=\"{}\">{}</segment>\n",
                    article.wire.len(),
                    part + 1,
                    article.message_id
                ));
            }
            xml.push_str("</segments></file>\n");
        }
        xml.push_str("</nzb>\n");
        xml.into_bytes()
    }

    pub fn standard_nzb(&self) -> Vec<u8> {
        self.nzb(&[0, 1, 2], quoted, POSTED)
    }
}

pub fn quoted(name: &str, n: usize, total: usize) -> String {
    format!("[{n}/{total}] - \"{name}\" yEnc (1/1)")
}

pub fn bare(name: &str, _: usize, _: usize) -> String {
    format!("Re: {name} yEnc")
}

pub fn server(id: &str, server: &LoopbackServer, priority: u32) -> ServerConfig {
    ServerConfig {
        id: id.to_owned(),
        name: format!("server {id}"),
        host: "127.0.0.1".to_owned(),
        port: server.addr().port(),
        tls: false,
        verify_cert: true,
        ip_preference: IpPreference::Any,
        connections: 4,
        priority,
        backbone: None,
        retention_days: None,
        expires_on: None,
        quota_bytes: None,
        username: "user".to_owned(),
        password: "hunter2-secret".to_owned(),
    }
}

pub async fn engine(dir: &Path, servers: Vec<ServerConfig>) -> Engine {
    Engine::new(EngineConfig::new(dir.to_path_buf(), servers))
        .await
        .expect("engine")
}

pub async fn import(engine: &Engine, nzb: &[u8], cache_key: Option<&str>) -> Release {
    engine
        .import(Some(nzb), cache_key, "Show.S01", None, ImportMode::Stream)
        .await
        .expect("import")
}

pub fn requests(engine: &Engine) -> u64 {
    engine
        .stats()
        .iter()
        .flat_map(|p| p.workloads.iter())
        .flat_map(|w| w.attempts.iter())
        .sum()
}

pub fn output(release: &Release, path: &str) -> usize {
    release
        .outputs
        .iter()
        .find(|o| o.path == path)
        .expect("output")
        .index
}

pub async fn read_at<R: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin>(
    reader: &mut R,
    at: u64,
    len: usize,
) -> Vec<u8> {
    reader
        .seek(std::io::SeekFrom::Start(at))
        .await
        .expect("seek");
    let mut buf = vec![0; len];
    reader.read_exact(&mut buf).await.expect("read");
    buf
}

pub async fn read_all(engine: &Engine, fixture: &Fixture) {
    let release = import(engine, &fixture.standard_nzb(), None).await;
    for out in &release.outputs {
        let mut all = Vec::new();
        engine
            .open(&release.key, out.index, kob_usenet::Purpose::Play)
            .unwrap()
            .read_to_end(&mut all)
            .await
            .unwrap();
        assert!(all == fixture.data(&out.path), "{}", out.path);
    }
}
