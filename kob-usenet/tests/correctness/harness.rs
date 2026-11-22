use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use kob_usenet::vendor::testkit::{Article, LoopbackServer};
use kob_usenet::{
    Engine, EngineConfig, Error, ImportMode, IpPreference, Purpose, Release, ReleaseKey,
    ServerConfig,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const READ_DEADLINE: Duration = Duration::from_secs(30);
const CHUNK: usize = 7 * 1024;

pub fn server(id: &str, addr: SocketAddr, priority: u32) -> ServerConfig {
    ServerConfig {
        id: id.to_owned(),
        name: format!("server {id}"),
        host: addr.ip().to_string(),
        port: addr.port(),
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
        password: "secret".to_owned(),
    }
}

pub struct Rig {
    pub engine: Engine,
    pub servers: Vec<LoopbackServer>,
    _dir: tempfile::TempDir,
}

impl Rig {
    pub async fn new(providers: Vec<Vec<Article>>) -> Self {
        let mut servers = Vec::new();
        for articles in providers {
            servers.push(LoopbackServer::start(articles).await.unwrap());
        }
        let configs = servers
            .iter()
            .enumerate()
            .map(|(i, s)| server(&format!("p{i}"), s.addr(), if i == 0 { 0 } else { 99 }))
            .collect();
        let (engine, dir) = engine(configs).await;
        Self {
            engine,
            servers,
            _dir: dir,
        }
    }

    pub async fn import(&self, nzb: &[u8]) -> Result<Release, Error> {
        import(&self.engine, nzb).await
    }
}

pub fn imported(result: Result<Release, Error>, case: &str) -> Option<Release> {
    result
        .inspect_err(|error| eprintln!("{case}: refused at import: {error}"))
        .ok()
}

pub async fn engine(servers: Vec<ServerConfig>) -> (Engine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig::new(dir.path().to_path_buf(), servers))
        .await
        .unwrap();
    (engine, dir)
}

pub async fn import(engine: &Engine, nzb: &[u8]) -> Result<Release, Error> {
    engine
        .import(Some(nzb), None, "Show.S01", None, ImportMode::Stream)
        .await
}

pub fn output(release: &Release, path: &str) -> Option<usize> {
    release
        .outputs
        .iter()
        .find(|o| Path::new(&o.path).file_name().and_then(|n| n.to_str()) == Some(path))
        .map(|o| o.index)
}

pub fn paths(release: &Release) -> Vec<&str> {
    release.outputs.iter().map(|o| o.path.as_str()).collect()
}

pub async fn read_checked(
    engine: &Engine,
    key: &ReleaseKey,
    index: usize,
    truth: &[u8],
    at: usize,
    len: usize,
) -> io::Result<()> {
    let mut reader = engine
        .open(key, index, Purpose::Play)
        .map_err(io::Error::other)?;
    assert_eq!(
        reader.len(),
        truth.len() as u64,
        "output {index} is listed at the wrong size"
    );
    let read = async {
        reader.seek(io::SeekFrom::Start(at as u64)).await?;
        let mut buf = vec![0; CHUNK];
        let mut pos = at;
        while pos < at + len {
            let want = CHUNK.min(at + len - pos);
            let n = reader.read(&mut buf[..want]).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("ended at {pos} of {}", truth.len()),
                ));
            }
            assert_clean(&buf[..n], truth, pos);
            pos += n;
        }
        Ok(())
    };
    tokio::time::timeout(READ_DEADLINE, read)
        .await
        .unwrap_or_else(|_| panic!("read of output {index} at {at}+{len} hung"))
}

fn assert_clean(got: &[u8], truth: &[u8], at: usize) {
    let expected = &truth[at..at + got.len()];
    if let Some(i) = got.iter().zip(expected).position(|(a, b)| a != b) {
        let bad = &got[i..];
        let zeros = bad.iter().take_while(|&&b| b == 0).count();
        panic!(
            "served wrong bytes at offset {}: {zeros} zero bytes follow, of {} delivered in this read",
            at + i,
            got.len()
        );
    }
}

pub async fn read_whole(
    engine: &Engine,
    key: &ReleaseKey,
    index: usize,
    truth: &[u8],
) -> io::Result<()> {
    read_checked(engine, key, index, truth, 0, truth.len()).await
}

pub async fn served(engine: &Engine, release: &Release, name: &str, truth: &[u8]) {
    let index =
        output(release, name).unwrap_or_else(|| panic!("{name} missing from {:?}", paths(release)));
    if let Err(error) = read_whole(engine, &release.key, index, truth).await {
        panic!("{name} was refused: {error}");
    }
}

pub async fn refused_or_absent(engine: &Engine, release: &Release, name: &str, truth: &[u8]) {
    let Some(index) = output(release, name) else {
        return;
    };
    assert!(
        read_whole(engine, &release.key, index, truth)
            .await
            .is_err(),
        "{name} was served in full although its data is not all posted"
    );
}
