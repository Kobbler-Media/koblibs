use crate::nzb;
use crate::obfuscated::hash;
use crate::post::{ARTICLE, Posted, video};
use crate::repair::Protected;
use crate::support::{Rig, assert_file, completed, failed, finish, listing};

const REAL: &str = "Clayface.2026.1080p.WEB.mkv";

fn obfuscated() -> String {
    format!("{}.mkv", hash(REAL))
}

struct Headless {
    release: Protected,
    hidden: Posted,
}

impl Headless {
    fn new(described: &[(&str, Vec<u8>)], volumes: &[(u32, u32)], claimed: Option<usize>) -> Self {
        let release = Protected::new(
            described
                .iter()
                .enumerate()
                .map(|(i, (name, data))| Posted::new(&format!("real{i}"), name, data.clone()))
                .collect(),
            "Clayface",
            volumes,
        );
        let data = described[0].1.clone();
        let hidden = match claimed {
            Some(claimed) => Posted::claiming("obf", &obfuscated(), data, claimed),
            None => Posted::new("obf", &obfuscated(), data),
        };
        Self { release, hidden }
    }

    fn posts(&self) -> Vec<&Posted> {
        [&self.hidden, &self.release.index]
            .into_iter()
            .chain(&self.release.volumes)
            .collect()
    }

    async fn rig(&self, lost: usize) -> Rig {
        let gone: Vec<&str> = (0..lost).map(|part| self.hidden.id(part)).collect();
        Rig::new(
            crate::post::articles(self.posts())
                .into_iter()
                .filter(|a| !gone.contains(&a.message_id.as_str()))
                .collect(),
        )
        .await
    }
}

struct Case {
    name: &'static str,
    lost: usize,
    claimed: Option<usize>,
    fetched: [usize; 4],
}

#[tokio::test]
async fn an_obfuscated_volume_missing_its_first_article_is_matched_by_size_and_repaired() {
    let len = 12 * ARTICLE + 500;
    let cases = [
        Case {
            name: "only the first article lost",
            lost: 1,
            claimed: None,
            fetched: [0, 0, 1, 0],
        },
        Case {
            name: "the second article lost too",
            lost: 2,
            claimed: None,
            fetched: [0, 0, 0, 2],
        },
        Case {
            name: "yEnc headers claiming a wrong size",
            lost: 1,
            claimed: Some(len + 3000),
            fetched: [0, 0, 1, 0],
        },
    ];
    for case in cases {
        let data = video(len, 91);
        let headless = Headless::new(
            &[(REAL, data.clone())],
            &[(0, 1), (1, 2), (3, 4), (7, 8)],
            case.claimed,
        );
        let rig = headless.rig(case.lost).await;
        for volume in &headless.release.volumes {
            rig.count_bodies(&volume.articles[1..]);
        }
        let engine = rig.engine().await;
        let spec = rig.spec("Clayface", nzb::of(&headless.posts()), None);
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();

        let status = finish(&engine, &id).await;
        assert!(status.error.is_none(), "{}: {:?}", case.name, status.error);
        let dir = completed(&status);
        assert_eq!(listing(&dir), [REAL], "{}", case.name);
        assert_file(&dir, REAL, &data);
        let fetched: Vec<usize> = headless
            .release
            .volumes
            .iter()
            .map(|v| rig.bodies(&v.articles[1..]))
            .collect();
        assert_eq!(fetched, case.fetched, "{}", case.name);
    }
}

#[tokio::test]
async fn a_volume_missing_its_first_article_fails_when_two_par2_files_fit_its_size() {
    let len = 12 * ARTICLE + 500;
    let headless = Headless::new(
        &[
            (REAL, video(len, 92)),
            ("Clayface.sample.mkv", video(len, 93)),
        ],
        &[(0, 8), (8, 16)],
        None,
    );
    let rig = headless.rig(1).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Clayface", nzb::of(&headless.posts()), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let error = failed(&finish(&engine, &id).await);
    assert!(
        error.contains("first article is missing") && error.contains(&obfuscated()),
        "{error}"
    );
}
