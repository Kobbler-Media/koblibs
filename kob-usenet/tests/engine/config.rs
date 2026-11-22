use kob_usenet::EngineConfig;
use kob_usenet::vendor::testkit::LoopbackServer;

use crate::support::server;

#[tokio::test]
async fn debug_output_never_contains_a_password() {
    let nntp = LoopbackServer::start(Vec::new()).await.unwrap();
    let config = server("a", &nntp, 0);
    let engine = EngineConfig::new("/tmp/x".into(), vec![config.clone()]);
    for printed in [
        format!("{config:?}"),
        format!("{engine:?}"),
        format!("{engine:#?}"),
    ] {
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("user"), "{printed}");
    }
}

#[test]
fn retention_counts_whole_seconds_with_the_last_day_included() {
    let kept = kob_usenet::ServerConfig {
        retention_days: Some(2),
        ..server_config()
    };
    let day = 86_400;
    assert!(kept.retains(1_000, 1_000 + 2 * day));
    assert!(!kept.retains(1_000, 1_000 + 2 * day + 1));
    assert!(
        kob_usenet::ServerConfig {
            retention_days: None,
            ..server_config()
        }
        .retains(0, i64::MAX / 2)
    );
}

fn server_config() -> kob_usenet::ServerConfig {
    kob_usenet::ServerConfig {
        id: "a".into(),
        name: "a".into(),
        host: "localhost".into(),
        port: 119,
        tls: false,
        verify_cert: true,
        ip_preference: kob_usenet::IpPreference::Any,
        connections: 1,
        priority: 0,
        backbone: None,
        retention_days: None,
        expires_on: None,
        quota_bytes: None,
        username: String::new(),
        password: String::new(),
    }
}
