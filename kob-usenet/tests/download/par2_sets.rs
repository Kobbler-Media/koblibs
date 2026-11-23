use crate::post::{ARTICLE, Posted, video};
use crate::support::{Rig, articles_without, assert_file, completed, failed, par2_set, run_job};

const SLICE: usize = ARTICLE / 2 / 4 * 4;
const FINE: usize = ARTICLE / 4 / 4 * 4;

fn season() -> (Posted, Posted) {
    (
        Posted::new("a", "Show.S01E01.mkv", video(8 * ARTICLE + 100, 3)),
        Posted::new("b", "Show.S01E02.mkv", video(8 * ARTICLE + 900, 4)),
    )
}

#[tokio::test]
async fn damage_in_any_par2_set_is_repaired_from_that_set() {
    for (damaged, part) in [(0, 2), (1, 2), (0, 0), (1, 0)] {
        let (a, b) = season();
        let (pa, pb) = (
            par2_set(&[&a], "A", SLICE, 100),
            par2_set(&[&b], "B", FINE, 100),
        );
        let gone = [&a, &b][damaged].id(part).to_owned();
        let mut posts = vec![&a, &b];
        posts.extend(&pa);
        posts.extend(&pb);
        let rig = Rig::new(articles_without(&posts, &[&gone])).await;
        let engine = rig.engine().await;
        let status = run_job(&rig, &engine, "Show", &posts).await;
        let dir = completed(&status);
        assert_file(&dir, &a.name, &a.data);
        assert_file(&dir, &b.name, &b.data);
    }
}

#[tokio::test]
async fn damage_in_both_sets_is_repaired_from_each() {
    let (a, b) = season();
    let (pa, pb) = (
        par2_set(&[&a], "A", SLICE, 100),
        par2_set(&[&b], "B", SLICE, 100),
    );
    let gone = [a.id(3).to_owned(), b.id(5).to_owned()];
    let mut posts = vec![&a, &b];
    posts.extend(&pa);
    posts.extend(&pb);
    let rig = Rig::new(articles_without(&posts, &[&gone[0], &gone[1]])).await;
    let engine = rig.engine().await;
    let dir = completed(&run_job(&rig, &engine, "Show", &posts).await);
    assert_file(&dir, &a.name, &a.data);
    assert_file(&dir, &b.name, &b.data);
}

#[tokio::test]
async fn damage_no_par2_set_describes_fails_the_job() {
    for part in [0, 2] {
        let (a, b) = season();
        let pa = par2_set(&[&a], "A", SLICE, 100);
        let gone = b.id(part).to_owned();
        let mut posts = vec![&a, &b];
        posts.extend(&pa);
        let rig = Rig::new(articles_without(&posts, &[&gone])).await;
        let engine = rig.engine().await;
        failed(&run_job(&rig, &engine, "Show", &posts).await);
    }
}

#[tokio::test]
async fn a_second_set_without_recovery_blocks_fails_the_job() {
    let (a, b) = season();
    let (pa, pb) = (
        par2_set(&[&a], "A", SLICE, 100),
        par2_set(&[&b], "B", SLICE, 100),
    );
    let gone = b.id(2).to_owned();
    let mut posts = vec![&a, &b];
    posts.extend(&pa);
    posts.push(&pb[0]);
    let rig = Rig::new(articles_without(&posts, &[&gone])).await;
    let engine = rig.engine().await;
    failed(&run_job(&rig, &engine, "Show", &posts).await);
}

#[tokio::test]
async fn a_decoy_set_describing_the_hole_cannot_complete_the_job() {
    for (decoy_slice, repaired) in [
        (FINE, true),
        (FINE / 2, true),
        (2 * SLICE, true),
        (SLICE, false),
    ] {
        let (a, b) = season();
        let mut holed = b.data.clone();
        holed[b.range(2)].fill(0);
        let decoy_file = Posted::new("decoy", &b.name, holed);
        let (pa, pb, decoy) = (
            par2_set(&[&a], "A", SLICE, 100),
            par2_set(&[&b], "B", SLICE, 100),
            par2_set(&[&decoy_file], "Decoy", decoy_slice, 60),
        );
        let gone = b.id(2).to_owned();
        let mut posts = vec![&a, &b];
        posts.extend(&pa);
        posts.extend(&pb);
        posts.extend(&decoy);
        let rig = Rig::new(articles_without(&posts, &[&gone])).await;
        let engine = rig.engine().await;
        let status = run_job(&rig, &engine, "Show", &posts).await;
        if repaired {
            assert_file(&completed(&status), &b.name, &b.data);
        } else {
            failed(&status);
        }
    }
}

#[tokio::test]
async fn damage_found_before_a_restart_still_needs_a_repair() {
    for protected in [true, false] {
        let (a, b) = season();
        let pb = par2_set(&[&b], "B", SLICE, 100);
        let gone = b.id(2).to_owned();
        let mut posts = vec![&a, &b];
        if protected {
            posts.extend(&pb);
        }
        let rig = Rig::new(articles_without(&posts, &[&gone])).await;
        let (arrived, release) = rig.nntp.hold_article(b.id(5));
        let engine = rig.engine().await;
        let spec = rig.spec("Show", crate::nzb::of(&posts), None);
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();
        tokio::select! {
            _ = arrived => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            _ = crate::support::finish(&engine, &id) => {}
        }
        drop(engine);
        drop(release);

        let engine = rig.engine().await;
        let status = crate::support::finish(&engine, &id).await;
        if protected {
            let dir = completed(&status);
            assert_file(&dir, &a.name, &a.data);
            assert_file(&dir, &b.name, &b.data);
        } else {
            failed(&status);
        }
    }
}

#[tokio::test]
async fn a_damaged_archive_member_with_no_par2_fails_the_job() {
    let film = video(6 * ARTICLE + 11, 5);
    let set = crate::rar::stored(
        &[crate::rar::Member {
            name: "Movie.mkv",
            data: &film,
        }],
        100 * ARTICLE,
    );
    let volume = Posted::new("rar", "Movie.rar", set.volumes[0].clone());
    let gone = volume.id(3).to_owned();
    let rig = Rig::new(articles_without(&[&volume], &[&gone])).await;
    let engine = rig.engine().await;
    failed(&run_job(&rig, &engine, "Movie", &[&volume]).await);
}

#[tokio::test]
async fn sets_describing_different_bytes_for_a_damaged_file_fail_the_job() {
    for (fill, real_recovers, decoy_first) in [
        (0xAB, false, false),
        (0xAB, false, true),
        (0xAB, true, false),
        (0xAB, true, true),
        (0x00, false, false),
        (0x00, false, true),
        (0x00, true, true),
    ] {
        let (a, b) = season();
        let mut other = b.data.clone();
        other[b.range(2)].fill(fill);
        let decoy_file = Posted::new("decoy", &b.name, other);
        let (pa, pb, decoy) = (
            par2_set(&[&a], "A", SLICE, 100),
            par2_set(&[&b], "B", SLICE, 100),
            par2_set(&[&decoy_file], "Decoy", FINE, 60),
        );
        let real: &[Posted] = if real_recovers { &pb } else { &pb[..1] };
        let mut posts = vec![&a, &b];
        posts.extend(&pa);
        if decoy_first {
            posts.extend(&decoy);
            posts.extend(real);
        } else {
            posts.extend(real);
            posts.extend(&decoy);
        }
        let gone = b.id(2).to_owned();
        let rig = Rig::new(articles_without(&posts, &[&gone])).await;
        let engine = rig.engine().await;
        let status = run_job(&rig, &engine, "Show", &posts).await;
        assert!(
            status.error.is_some(),
            "fill={fill} real_recovers={real_recovers} decoy_first={decoy_first}: {:?}",
            status.state
        );
        failed(&status);
    }
}
