use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kob_usenet::vendor::testkit::{Article, Impairment, LoopbackServer, post};
use kob_usenet::{Engine, EngineConfig, ImportMode, JobSpec, JobState, Purpose, Release};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::rig::{media, single_nzb};
use crate::support::{POSTED, quoted, server};

const CONNECTIONS: usize = 32;
const LATENCY: Duration = Duration::from_millis(100);
const PER_CONNECTION: u64 = 1_500_000;
const FILM_BYTES: usize = 900_000_000;
const LIBRARY_BYTES: usize = 1_500_000_000;
const SEEK_EVERY: Duration = Duration::from_millis(400);
const SEEK_READ: usize = 256 * 1024;
const REPAIR_BYTES: usize = 420_000_000;
const SOLVING_CORES: f64 = 2.0;
const CLOCK_TICKS: f64 = 100.0;

fn article_size() -> usize {
    std::env::var("KOB_MEASURE_ARTICLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(700_000)
}

fn repairing() -> bool {
    std::env::var_os("KOB_MEASURE_REPAIR").is_some()
}

fn repair_nzb_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("kob-measure-{}.nzb", std::process::id()))
}

// A set whose solve spends the repair budget: a fifth of its slices are gone.
fn damaged_release(article: usize, nzb: &std::path::Path) -> Vec<Article> {
    let dir = tempfile::tempdir().unwrap();
    let data = media(REPAIR_BYTES, 233);
    std::fs::write(dir.path().join("Repair.2026.mkv"), &data).unwrap();
    let status = Command::new("par2")
        .current_dir(dir.path())
        .args([
            "create",
            "-q",
            "-q",
            "-r25",
            "-n4",
            "-s1048576",
            "Repair.par2",
        ])
        .arg("Repair.2026.mkv")
        .status()
        .unwrap();
    assert!(status.success());
    let mut posts = vec![("rep".to_owned(), "Repair.2026.mkv".to_owned(), data)];
    let mut volumes: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".par2"))
        .collect();
    volumes.sort();
    for (i, name) in volumes.into_iter().enumerate() {
        let data = std::fs::read(dir.path().join(&name)).unwrap();
        posts.push((format!("rpar{i}"), name, data));
    }
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n",
    );
    let mut kept = Vec::new();
    for (n, (prefix, name, data)) in posts.iter().enumerate() {
        let articles = post(prefix, name, data, article);
        xml.push_str(&format!(
            "<file poster=\"p\" date=\"{POSTED}\" subject=\"{}\"><groups><group>a.b</group></groups><segments>\n",
            quoted(name, n + 1, posts.len()).replace('"', "&quot;")
        ));
        for (part, posted) in articles.iter().enumerate() {
            xml.push_str(&format!(
                "<segment bytes=\"{}\" number=\"{}\">{}</segment>\n",
                posted.wire.len(),
                part + 1,
                posted.message_id
            ));
        }
        xml.push_str("</segments></file>\n");
        let lost = n == 0;
        kept.extend(
            articles
                .into_iter()
                .enumerate()
                .filter(|(part, _)| !lost || part % 5 != 2)
                .map(|(_, a)| a),
        );
    }
    xml.push_str("</nzb>\n");
    std::fs::write(nzb, xml).unwrap();
    kept
}

fn seeking() -> bool {
    std::env::var_os("KOB_MEASURE_NO_SEEKS").is_none()
}

// The articles live in a child process so the parent's peak RSS is the engine's alone.
struct Provider {
    child: Child,
    port: u16,
}

impl Provider {
    fn spawn() -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        if repairing() {
            command.env("KOB_MEASURE_NZB", repair_nzb_path());
        }
        let mut child = command
            .args(["measure::serve", "--exact", "--ignored", "--nocapture"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let port = lines
            .by_ref()
            .find_map(|line| line.unwrap().split("PORT=").nth(1).map(str::to_owned))
            .unwrap();
        // libtest's 60-second notice kills the child if nobody reads its stdout.
        std::thread::spawn(move || lines.for_each(drop));
        Self {
            child,
            port: port.trim().parse().unwrap(),
        }
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "child process of the measurement scenario"]
async fn serve() {
    let article = article_size();
    let mut articles = post("film", "Film.2026.mkv", &media(FILM_BYTES, 239), article);
    articles.extend(post(
        "lib",
        "Library.2026.mkv",
        &media(LIBRARY_BYTES, 251),
        article,
    ));
    if let Some(path) = std::env::var_os("KOB_MEASURE_NZB") {
        articles.extend(damaged_release(article, std::path::Path::new(&path)));
    }
    let nntp = LoopbackServer::start_impaired(
        articles,
        Impairment {
            response_delay: LATENCY,
            bandwidth: Some(PER_CONNECTION),
            ..Impairment::default()
        },
    )
    .await
    .unwrap();
    println!("PORT={}", nntp.addr().port());
    std::io::stdout().flush().unwrap();
    tokio::task::spawn_blocking(|| {
        let mut sink = [0; 1];
        let _ = std::io::stdin().read(&mut sink);
    })
    .await
    .unwrap();
}

fn peak_rss_mib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
    let kb: u64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    kb / 1024
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

async fn seeks(engine: Arc<Engine>, film: Release, done: Arc<AtomicBool>) -> Vec<f64> {
    let mut timings = Vec::new();
    let mut state = 0x9E37_79B9_u64;
    while !done.load(Ordering::Relaxed) {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let mut reader = engine.open(&film.key, 0, Purpose::Play).unwrap();
        let at = (state >> 16) % (reader.len() - SEEK_READ as u64);
        reader.set_end(at + SEEK_READ as u64);
        let started = Instant::now();
        reader.seek(std::io::SeekFrom::Start(at)).await.unwrap();
        let mut buf = vec![0; SEEK_READ];
        reader.read_exact(&mut buf).await.unwrap();
        timings.push(started.elapsed().as_secs_f64());
        drop(reader);
        tokio::time::sleep(SEEK_EVERY).await;
    }
    timings
}

fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<&str> = stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    let ticks: f64 = fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap();
    ticks / CLOCK_TICKS
}

async fn solve_started(engine: &Engine) -> bool {
    let waited = Instant::now();
    while waited.elapsed() < Duration::from_mins(3) {
        if engine.job("damaged").unwrap().state != JobState::Repairing {
            return false;
        }
        let (cpu, at) = (cpu_seconds(), Instant::now());
        tokio::time::sleep(Duration::from_millis(500)).await;
        if (cpu_seconds() - cpu) / at.elapsed().as_secs_f64() >= SOLVING_CORES {
            return true;
        }
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "measurement: run alone, with --release"]
async fn scenario() {
    let article = article_size();
    let provider = Provider::spawn();
    let dir = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let mut account = server("a", &LoopbackServer::start(Vec::new()).await.unwrap(), 0);
    account.port = provider.port;
    account.connections = CONNECTIONS;
    let config = EngineConfig::new(dir.path().to_path_buf(), vec![account]);
    let engine = Arc::new(Engine::new(config).await.unwrap());
    let film_parts = FILM_BYTES.div_ceil(article);
    let film = engine
        .import(
            Some(&single_nzb("film", "Film.2026.mkv", film_parts, article)),
            None,
            "Film",
            None,
            ImportMode::Stream,
        )
        .await
        .unwrap();
    engine
        .add_job(JobSpec {
            id: "library".into(),
            name: "Library.2026".into(),
            category: "movies".into(),
            nzb: single_nzb(
                "lib",
                "Library.2026.mkv",
                LIBRARY_BYTES.div_ceil(article),
                article,
            ),
            password: None,
            dest_dir: dest.path().to_path_buf(),
        })
        .await
        .unwrap();
    while engine.job("library").unwrap().bytes_done < 20 * article as u64 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    if repairing() {
        let nzb = std::fs::read(repair_nzb_path()).unwrap();
        std::fs::remove_file(repair_nzb_path()).unwrap();
        engine
            .add_job(JobSpec {
                id: "damaged".into(),
                name: "Repair.2026".into(),
                category: "movies".into(),
                nzb,
                password: None,
                dest_dir: dest.path().to_path_buf(),
            })
            .await
            .unwrap();
        let waited = Instant::now();
        loop {
            let status = engine.job("damaged").unwrap();
            match status.state {
                JobState::Repairing => break,
                JobState::Failed | JobState::Completed => panic!("{status:?}"),
                _ if waited.elapsed() > Duration::from_mins(4) => panic!("{status:?}"),
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    let solving = if repairing() {
        solve_started(&engine).await
    } else {
        false
    };

    let done = Arc::new(AtomicBool::new(false));
    let seeker = seeking()
        .then(|| tokio::spawn(seeks(Arc::clone(&engine), film.clone(), Arc::clone(&done))));
    let queued_before = engine.job("library").unwrap().bytes_done;
    let started = Instant::now();
    let mut reader = engine.open(&film.key, 0, Purpose::Cache).unwrap();
    let mut buf = vec![0; 256 * 1024];
    let mut read = 0_u64;
    loop {
        let n = reader.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        read += n as u64;
    }
    let took = started.elapsed().as_secs_f64();
    let queued_at_end = engine.job("library").unwrap().bytes_done;
    drop(reader);
    done.store(true, Ordering::Relaxed);
    let mut timings = match seeker {
        Some(seeker) => seeker.await.unwrap(),
        None => Vec::new(),
    };
    timings.sort_by(f64::total_cmp);

    let repair_state = repairing().then(|| (solving, engine.job("damaged").unwrap().state));
    let cache_ended = Instant::now();
    let resumed_from = engine.job("library").unwrap().bytes_done;
    let resume = loop {
        let status = engine.job("library").unwrap();
        if status.bytes_done >= resumed_from + 4 * article as u64
            || status.state != JobState::Downloading
        {
            break cache_ended.elapsed().as_secs_f64();
        }
        if cache_ended.elapsed() > Duration::from_secs(30) {
            break f64::INFINITY;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let before = engine.job("library").unwrap().bytes_done;
    tokio::time::sleep(Duration::from_secs(4)).await;
    let after_rate = (engine.job("library").unwrap().bytes_done - before) as f64 / 4e6;
    println!(
        "RESULT article={article} seeks={} p50={:.2} p95={:.2} p99={:.2} cache_mbs={:.1} queue_mbs={:.1} cache_secs={took:.1} resume_s={resume:.2} queue_after_mbs={after_rate:.1} peak_rss_mib={} repair_after={repair_state:?}",
        timings.len(),
        percentile(&timings, 0.5),
        percentile(&timings, 0.95),
        percentile(&timings, 0.99),
        read as f64 / took / 1e6,
        (queued_at_end - queued_before) as f64 / took / 1e6,
        peak_rss_mib(),
    );
}
