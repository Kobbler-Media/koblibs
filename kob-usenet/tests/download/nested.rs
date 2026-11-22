use kob_usenet::vendor::testkit::Article;

use crate::nzb;
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, finish};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_nzb_holding_only_another_nzb_downloads_the_inner_release() {
    let film = Posted::new("film", "Movie.2026.mkv", video(5 * ARTICLE + 321, 71));
    let inner = Posted::new("inner", "Movie.2026.nzb", nzb::of(&[&film]));
    let articles: Vec<Article> = film
        .articles
        .iter()
        .chain(&inner.articles)
        .cloned()
        .collect();
    let rig = Rig::new(articles).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Movie", nzb::of(&[&inner]), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    assert_file(&dir, &film.name, &film.data);
    assert!(!dir.join(&inner.name).exists());
}
