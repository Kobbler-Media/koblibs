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
