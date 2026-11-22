use kob_usenet::vendor::testkit::Article;

use crate::nzb;
use crate::par2::{self, Set};
use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, assert_file, completed, finish};

pub struct Protected {
    pub files: Vec<Posted>,
    pub index: Posted,
    pub volumes: Vec<Posted>,
}

impl Protected {
    pub fn new(files: Vec<Posted>, set_name: &str, volumes: &[(u32, u32)]) -> Self {
        let set = Set::new(
            &files
                .iter()
                .map(|f| (f.name.as_str(), f.data.as_slice()))
                .collect::<Vec<_>>(),
        );
        let index = Posted::new("idx", &format!("{set_name}.par2"), set.index());
        let volumes = volumes
            .iter()
            .map(|&(first, count)| {
                Posted::new(
                    &format!("vol{first}"),
                    &par2::volume_name(set_name, first, count),
                    set.volume(first, count),
                )
            })
            .collect();
        Self {
            files,
            index,
            volumes,
        }
    }

    pub fn posts(&self) -> Vec<&Posted> {
        self.files
            .iter()
            .chain([&self.index])
            .chain(&self.volumes)
            .collect()
    }

    pub fn articles_without(&self, missing: &[&str]) -> Vec<Article> {
        self.posts()
            .into_iter()
            .flat_map(|p| p.articles.iter().cloned())
            .filter(|a| !missing.contains(&a.message_id.as_str()))
            .collect()
    }
}

#[tokio::test]
async fn missing_articles_within_par2_capacity_are_repaired_from_only_the_recovery_they_need() {
    let release = Protected::new(
        vec![
            Posted::new("e01", "Show.S01E01.mkv", video(12 * ARTICLE + 500, 31)),
            Posted::new("e02", "Show.S01E02.mkv", video(10 * ARTICLE + 33, 32)),
        ],
        "Show",
        &[(0, 2), (2, 8), (10, 16)],
    );
    let gone = release.files[0].id(4).to_owned();
    let rig = Rig::new(release.articles_without(&[&gone])).await;
    let recovery: Vec<Article> = release
        .volumes
        .iter()
        .flat_map(|v| v.articles.iter().skip(1).cloned())
        .collect();
    rig.count_bodies(&recovery);
    let engine = rig.engine().await;
    let spec = rig.spec("Show", nzb::of(&release.posts()), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let dir = completed(&finish(&engine, &id).await);
    for file in &release.files {
        assert_file(&dir, &file.name, &file.data);
    }
    let fetched: Vec<usize> = release
        .volumes
        .iter()
        .map(|v| rig.bodies(&v.articles[1..]))
        .collect();
    assert_eq!(
        fetched,
        [0, 1, 0],
        "one missing article is four slices: the smallest volume with four blocks holds them in its first two articles"
    );
}
