use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use kob_usenet::vendor::testkit::LoopbackServer;
use kob_usenet::{Engine, Purpose, Release};
use tokio::io::AsyncReadExt;

use crate::support::{Fixture, engine, import, output, server};

struct Peak {
    seen: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Peak {
    fn watch(nntp: &Arc<LoopbackServer>) -> Self {
        let (seen, stop) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let (nntp, s, done) = (Arc::clone(nntp), Arc::clone(&seen), Arc::clone(&stop));
        tokio::spawn(async move {
            while !done.load(Ordering::Relaxed) {
                s.fetch_max(nntp.open_connections(), Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        Self { seen, stop }
    }

    fn stop(self) -> usize {
        self.stop.store(true, Ordering::Relaxed);
        self.seen.load(Ordering::Relaxed)
    }
}

async fn read(engine: &Engine, release: &Release, fixture: &Fixture, path: &str) {
    let mut all = Vec::new();
    engine
        .open(&release.key, output(release, path), Purpose::Play)
        .unwrap()
        .read_to_end(&mut all)
        .await
        .unwrap();
    assert!(all == fixture.data(path), "{path}");
}

async fn drained(nntp: &LoopbackServer, cap: usize) {
    tokio::time::timeout(Duration::from_secs(90), async {
        while nntp.open_connections() > cap {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("connections over the new cap close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconfiguring_never_holds_more_connections_than_the_account_allows() {
    let fixture = Fixture::season();
    for change in ["rename", "password", "connections"] {
        let nntp = Arc::new(LoopbackServer::start(fixture.articles()).await.unwrap());
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
        let release = import(&engine, &fixture.standard_nzb(), None).await;
        read(&engine, &release, &fixture, "Show.S01E01.mkv").await;
        assert!(nntp.peak_connections() <= 4, "{change}");
        let mut changed = server("a", &nntp, 0);
        match change {
            "rename" => changed.name = "renamed".into(),
            "password" => changed.password = "rotated".into(),
            _ => changed.connections = 3,
        }
        engine.reconfigure(vec![changed.clone()]).unwrap();
        read(&engine, &release, &fixture, "Show.S01E02.mkv").await;
        assert!(
            nntp.peak_connections() <= 4,
            "{change}: {} over the account cap",
            nntp.peak_connections()
        );
        drained(&nntp, changed.connections).await;
        let peak = Peak::watch(&nntp);
        read(&engine, &release, &fixture, "Show.S01E01.mkv").await;
        read(&engine, &release, &fixture, "Show.S01E02.mkv").await;
        assert!(
            peak.stop() <= changed.connections,
            "{change}: the new cap holds after the old pool drains"
        );
        let name = if change == "rename" {
            "renamed"
        } else {
            "server a"
        };
        assert_eq!(engine.stats()[0].name, name);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raising_connections_cancels_a_pending_reclaim_instead_of_adding_permits() {
    let fixture = Fixture::season();
    let nntp = Arc::new(LoopbackServer::start(fixture.articles()).await.unwrap());
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp, 0)]).await;
    let release = import(&engine, &fixture.standard_nzb(), None).await;
    read(&engine, &release, &fixture, "Show.S01E01.mkv").await;
    let peak = Peak::watch(&nntp);

    for connections in [2, 3] {
        let mut changed = server("a", &nntp, 0);
        changed.connections = connections;
        engine.reconfigure(vec![changed]).unwrap();
    }
    read(&engine, &release, &fixture, "Show.S01E02.mkv").await;
    assert!(
        peak.stop() <= 4,
        "never more than the old cap while it drains"
    );
    drained(&nntp, 3).await;
    let peak = Peak::watch(&nntp);
    read(&engine, &release, &fixture, "Show.S01E01.mkv").await;
    assert!(peak.stop() <= 3, "the raised cap of 3 holds once drained");
}
