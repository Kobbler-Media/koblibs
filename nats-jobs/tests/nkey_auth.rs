use nats_jobs::{NatsClient, NatsConfig};
use nkeys::KeyPair;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const NATS_IMAGE: &str = "nats:2.15.2-alpine";

struct NkeyServer {
    container: String,
    config_path: PathBuf,
    url: String,
}

impl NkeyServer {
    fn start(user_public_key: &str) -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let container = format!("nats-jobs-nkey-test-{id}");
        let config_path = std::env::temp_dir().join(format!("{container}.conf"));
        std::fs::write(
            &config_path,
            format!(
                "jetstream {{ store_dir: /tmp/js }}\nauthorization {{ users = [ {{ nkey: {user_public_key} }} ] }}\n"
            ),
        )
        .expect("write nats config");

        let run = Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &container,
                "-p",
                "127.0.0.1::4222",
                "-v",
            ])
            .arg(format!("{}:/etc/nats/nkey.conf:ro", config_path.display()))
            .args([NATS_IMAGE, "-c", "/etc/nats/nkey.conf"])
            .output()
            .expect("docker run");
        let mut server = Self {
            container,
            config_path,
            url: String::new(),
        };
        assert!(
            run.status.success(),
            "docker run failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );

        let port = Command::new("docker")
            .args(["port", &server.container, "4222/tcp"])
            .output()
            .expect("docker port");
        let mapping = String::from_utf8_lossy(&port.stdout);
        let host_port = mapping
            .lines()
            .next()
            .and_then(|line| line.rsplit(':').next())
            .unwrap_or_else(|| panic!("no port mapping in {mapping:?}"))
            .trim()
            .to_string();

        server.url = format!("nats://127.0.0.1:{host_port}");
        server
    }

    fn wait_ready(&self) {
        for _ in 0..100 {
            let logs = Command::new("docker")
                .args(["logs", &self.container])
                .output()
                .expect("docker logs");
            if String::from_utf8_lossy(&logs.stderr).contains("Server is ready") {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("nats-server in {} never became ready", self.container);
    }
}

impl Drop for NkeyServer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.container])
            .output();
        let _ = std::fs::remove_file(&self.config_path);
    }
}

fn docker_available() -> bool {
    Command::new("docker")
        .arg("info")
        .output()
        .is_ok_and(|out| out.status.success())
}

async fn connects(url: &str, nkey_seed: Option<String>) -> bool {
    let config = NatsConfig {
        nkey_seed,
        ..NatsConfig::new(url, "nats-jobs-nkey-test")
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(5), NatsClient::connect(&config)).await,
        Ok(Ok(_))
    )
}

#[tokio::test]
async fn nkey_seed_authenticates_against_nkey_only_server() {
    if !docker_available() {
        eprintln!("docker unavailable, skipping");
        return;
    }
    let user = KeyPair::new_user();
    let stranger = KeyPair::new_user();
    let server = NkeyServer::start(&user.public_key());
    server.wait_ready();

    let cases = [
        ("matching seed", Some(user.seed().unwrap()), true),
        ("no seed", None, false),
        ("wrong seed", Some(stranger.seed().unwrap()), false),
    ];
    for (name, seed, expected) in cases {
        assert_eq!(connects(&server.url, seed).await, expected, "{name}");
    }
}
