use crate::nzb;
use crate::post::{ARTICLE, Posted, articles, noise, video};
use crate::rar::{self, Member};
use crate::support::{Rig, assert_file, completed, failed, finish, listing};

struct Release {
    loose: Vec<Posted>,
    volumes: Vec<Posted>,
    member: Vec<u8>,
}

fn release(corrupt_member: bool) -> Release {
    let loose = vec![
        Posted::new("ep", "Show.S01E01.mkv", video(5 * ARTICLE + 123, 1)),
        Posted::new("sub", "Show.S01E01.en.srt", noise(20 * 1024, 2)),
    ];
    let member = video(20 * ARTICLE + 77, 3);
    let mut set = rar::stored(
        &[Member {
            name: "Movie.2020.mkv",
            data: &member,
        }],
        9 * ARTICLE,
    );
    if corrupt_member {
        let piece = &set.pieces[0][0];
        set.volumes[piece.volume][piece.at + 5000] ^= 0xff;
    }
    let count = set.volumes.len();
    let volumes = set
        .volumes
        .into_iter()
        .enumerate()
        .map(|(i, data)| {
            Posted::new(
                &format!("rar{i}"),
                &rar::volume_name("Movie", i, count),
                data,
            )
        })
        .collect();
    Release {
        loose,
        volumes,
        member,
    }
}

impl Release {
    fn posts(&self) -> Vec<&Posted> {
        self.loose.iter().chain(&self.volumes).collect()
    }
}

#[tokio::test]
async fn loose_files_and_a_stored_rar_member_land_byte_exact_in_the_job_folder() {
    let release = release(false);
    let rig = Rig::new(articles(release.posts())).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Mixed", nzb::of(&release.posts()), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let status = finish(&engine, &id).await;
    let dir = completed(&status);
    assert_eq!(dir, rig.dest.path().join("Mixed"));
    assert_eq!(
        listing(&dir),
        ["Movie.2020.mkv", "Show.S01E01.en.srt", "Show.S01E01.mkv"]
    );
    for post in &release.loose {
        assert_file(&dir, &post.name, &post.data);
    }
    assert_file(&dir, "Movie.2020.mkv", &release.member);
    assert_eq!(status.bytes_done, status.bytes_total);
    assert!(!rig.dest.path().join(".incomplete").join("Mixed").exists());
    assert_eq!(engine.jobs(), vec![status]);
}

#[tokio::test]
async fn a_stored_member_that_disagrees_with_its_rar_crc_never_completes() {
    let release = release(true);
    let rig = Rig::new(articles(release.posts())).await;
    let engine = rig.engine().await;
    let spec = rig.spec("Bad", nzb::of(&release.posts()), None);
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let error = failed(&finish(&engine, &id).await);
    assert!(error.to_lowercase().contains("crc"), "{error}");
    assert!(!rig.dest.path().join("Bad").exists());
}
