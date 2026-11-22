use std::time::Duration;

use kob_usenet::vendor::testkit::{Article, Impairment, LoopbackServer, post};
use kob_usenet::{Engine, EngineConfig, JobSpec, JobState, ServerConfig};

use crate::support::{ARTICLE, Fixture, POSTED, engine, quoted, server};

const LIBRARY_ARTICLES: usize = 1500;
// Connections drain their pipelined download requests before a higher lane gets them.
// Timing tests share the machine with nothing but each other, one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

pub const DRAIN: Duration = Duration::from_millis(500);

pub struct Loaded {
    pub fixture: Fixture,
    pub nntp: LoopbackServer,
    backup: Option<LoopbackServer>,
    library: Vec<u8>,
    _dir: tempfile::TempDir,
    dest: tempfile::TempDir,
}

impl Loaded {
    pub async fn new(extra: Vec<Article>) -> Self {
        let fixture = Fixture::season();
        let mut library: Vec<u8> = (0..LIBRARY_ARTICLES * ARTICLE)
            .map(|i| (i % 251) as u8)
            .collect();
        library[..4].copy_from_slice(&[0x1A, 0x45, 0xDF, 0xA3]);
        let mut articles = fixture.articles();
        articles.extend(post("lib", "Library.2026.mkv", &library, ARTICLE));
        articles.extend(extra);
        let nntp = LoopbackServer::start_impaired(
            articles,
            Impairment {
                response_delay: Duration::from_millis(20),
                bandwidth: Some(1_000_000),
                ..Impairment::default()
            },
        )
        .await
        .unwrap();
        Self {
            fixture,
            nntp,
            backup: None,
            library,
            _dir: tempfile::tempdir().unwrap(),
            dest: tempfile::tempdir().unwrap(),
        }
    }

    pub async fn with_backup(mut self) -> Self {
        let mut articles = self.fixture.articles();
        articles.extend(post("lib", "Library.2026.mkv", &self.library, ARTICLE));
        self.backup = Some(LoopbackServer::start(articles).await.unwrap());
        self
    }

    pub fn backup_bodies(&self) -> usize {
        self.backup
            .as_ref()
            .map_or(0, LoopbackServer::body_commands)
    }

    fn servers(&self, connections: usize) -> Vec<ServerConfig> {
        let mut servers = vec![self.server(connections)];
        servers.extend(self.backup.as_ref().map(|backup| ServerConfig {
            connections: 32,
            ..server("backup", backup, 99)
        }));
        servers
    }

    pub fn server(&self, connections: usize) -> ServerConfig {
        ServerConfig {
            connections,
            ..server("a", &self.nntp, 0)
        }
    }

    pub async fn engine(&self) -> Engine {
        self.engine_with(4).await
    }

    pub async fn engine_with(&self, connections: usize) -> Engine {
        engine(self._dir.path(), self.servers(connections)).await
    }

    pub async fn engine_reading_ahead(&self, connections: usize, read_ahead: u64) -> Engine {
        let mut config =
            EngineConfig::new(self._dir.path().to_path_buf(), self.servers(connections));
        config.read_ahead_bytes = read_ahead;
        Engine::new(config).await.unwrap()
    }

    fn library_nzb(&self) -> Vec<u8> {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n",
        );
        xml.push_str(&format!(
            "<file poster=\"p\" date=\"{POSTED}\" subject=\"{}\"><groups><group>a.b</group></groups><segments>\n",
            quoted("Library.2026.mkv", 1, 1).replace('"', "&quot;")
        ));
        for part in 0..self.library.len().div_ceil(ARTICLE) {
            xml.push_str(&format!(
                "<segment bytes=\"{ARTICLE}\" number=\"{}\">lib-{part}@loopback</segment>\n",
                part + 1
            ));
        }
        xml.push_str("</segments></file>\n</nzb>\n");
        xml.into_bytes()
    }

    pub async fn start_download(&self, engine: &Engine) {
        self.start_downloads(engine, 1).await;
    }

    pub async fn start_downloads(&self, engine: &Engine, count: usize) {
        for n in 1..count {
            engine
                .add_job(JobSpec {
                    id: format!("library-{n}"),
                    name: format!("Library.2026.{n}"),
                    category: "movies".into(),
                    nzb: self.library_nzb(),
                    password: None,
                    dest_dir: self.dest.path().to_path_buf(),
                })
                .await
                .unwrap();
        }
        engine
            .add_job(JobSpec {
                id: "library".into(),
                name: "Library.2026".into(),
                category: "movies".into(),
                nzb: self.library_nzb(),
                password: None,
                dest_dir: self.dest.path().to_path_buf(),
            })
            .await
            .unwrap();
        for _ in 0..1000 {
            let status = engine.job("library").unwrap();
            if status.state == JobState::Downloading && status.segments_done > 20 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the download never started: {:?}", engine.job("library"));
    }
}

pub async fn within<T>(what: &str, work: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), work)
        .await
        .unwrap_or_else(|_| panic!("{what} starved behind the download"))
}

pub fn downloading(engine: &Engine) -> bool {
    engine.job("library").unwrap().state == JobState::Downloading
}
