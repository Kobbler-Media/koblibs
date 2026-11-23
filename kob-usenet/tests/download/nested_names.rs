use std::path::Path;

use crate::post::{ARTICLE, Posted, noise};
use crate::support::{Rig, assert_file, completed, extractor_on_path, listing, run_job, sevenzip};

#[tokio::test]
async fn an_archive_holding_its_own_name_is_unpacked_at_every_level() {
    if !extractor_on_path() {
        return;
    }
    for levels in [1, 2] {
        let work = tempfile::tempdir().unwrap();
        let movie = vec![7u8; 3 * ARTICLE];
        std::fs::write(work.path().join("movie.bin"), &movie).unwrap();
        sevenzip(work.path(), &["a", "-mx5", "a.7z", "movie.bin"]);
        std::fs::remove_file(work.path().join("movie.bin")).unwrap();
        for level in 0..levels {
            let pad = format!("pad{level}.txt");
            std::fs::write(work.path().join(&pad), vec![9u8; ARTICLE]).unwrap();
            sevenzip(work.path(), &["a", "-mx5", "next.7z", "a.7z", &pad]);
            std::fs::remove_file(work.path().join("a.7z")).unwrap();
            std::fs::remove_file(work.path().join(&pad)).unwrap();
            std::fs::rename(work.path().join("next.7z"), work.path().join("a.7z")).unwrap();
        }
        let post = Posted::new(
            "n",
            "a.7z",
            std::fs::read(work.path().join("a.7z")).unwrap(),
        );
        let rig = Rig::new(post.articles.clone()).await;
        let engine = rig.engine().await;
        let dir = completed(&run_job(&rig, &engine, "Nested", &[&post]).await);
        assert_file(&dir, "movie.bin", &movie);
        let names = listing(&dir);
        assert!(!names.iter().any(|n| n.ends_with(".7z")), "{names:?}");
        for level in 0..levels {
            assert!(names.contains(&format!("pad{level}.txt")), "{names:?}");
        }
    }
}

fn packed(work: &Path, archive: &str, members: &[(&str, &[u8])]) -> Vec<u8> {
    let names: Vec<&str> = members.iter().map(|(name, _)| *name).collect();
    for (name, data) in members {
        std::fs::write(work.join(name), data).unwrap();
    }
    let mut args = vec!["a", "-mx5", archive];
    args.extend(&names);
    sevenzip(work, &args);
    let bytes = std::fs::read(work.join(archive)).unwrap();
    std::fs::remove_file(work.join(archive)).unwrap();
    for name in names {
        std::fs::remove_file(work.join(name)).unwrap();
    }
    bytes
}

fn holding(dir: &Path, wanted: &[u8]) -> bool {
    listing(dir)
        .iter()
        .any(|name| std::fs::read(dir.join(name)).is_ok_and(|data| data == wanted))
}

#[tokio::test]
async fn an_extracted_archive_never_replaces_a_different_sibling_archive() {
    if !extractor_on_path() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let (inner, waiting) = (vec![1u8; 3 * ARTICLE], vec![2u8; 3 * ARTICLE]);
    let nested = packed(work.path(), "b.7z", &[("inner.bin", &inner)]);
    let pad = noise(ARTICLE, 41);
    let outer = packed(work.path(), "a.7z", &[("b.7z", &nested), ("pad.bin", &pad)]);
    let sibling = packed(work.path(), "b.7z", &[("sibling.bin", &waiting)]);
    let (a, b) = (
        Posted::new("a", "a.7z", outer),
        Posted::new("b", "b.7z", sibling),
    );
    let rig = Rig::new(a.articles.iter().chain(&b.articles).cloned().collect()).await;
    let engine = rig.engine().await;
    let dir = completed(&run_job(&rig, &engine, "Sibling", &[&a, &b]).await);
    assert_file(&dir, "inner.bin", &inner);
    assert_file(&dir, "sibling.bin", &waiting);
    assert_file(&dir, "pad.bin", &pad);
    let names = listing(&dir);
    assert!(!names.iter().any(|n| n.contains(".7z")), "{names:?}");
}

#[tokio::test]
async fn archives_extracting_one_name_with_different_content_keep_both() {
    if !extractor_on_path() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let (first, second) = (vec![3u8; 3 * ARTICLE], vec![4u8; 3 * ARTICLE]);
    let one = packed(work.path(), "one.7z", &[("movie.bin", &first)]);
    let two = packed(work.path(), "two.7z", &[("movie.bin", &second)]);
    let (one, two) = (
        Posted::new("one", "one.7z", one),
        Posted::new("two", "two.7z", two),
    );
    let rig = Rig::new(one.articles.iter().chain(&two.articles).cloned().collect()).await;
    let engine = rig.engine().await;
    let dir = completed(&run_job(&rig, &engine, "Both", &[&one, &two]).await);
    assert!(
        holding(&dir, &first) && holding(&dir, &second),
        "{:?}",
        listing(&dir)
    );
    assert_eq!(listing(&dir).len(), 2, "{:?}", listing(&dir));
}

#[tokio::test]
async fn archives_extracting_identical_bytes_keep_one_copy() {
    if !extractor_on_path() {
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let same = vec![5u8; 3 * ARTICLE];
    let (x, y) = (vec![6u8; ARTICLE], vec![7u8; ARTICLE]);
    let one = packed(
        work.path(),
        "one.7z",
        &[("movie.bin", &same), ("x.bin", &x)],
    );
    let two = packed(
        work.path(),
        "two.7z",
        &[("movie.bin", &same), ("y.bin", &y)],
    );
    let (one, two) = (
        Posted::new("one", "one.7z", one),
        Posted::new("two", "two.7z", two),
    );
    let rig = Rig::new(one.articles.iter().chain(&two.articles).cloned().collect()).await;
    let engine = rig.engine().await;
    let dir = completed(&run_job(&rig, &engine, "Same", &[&one, &two]).await);
    assert_eq!(listing(&dir), ["movie.bin", "x.bin", "y.bin"]);
    assert_file(&dir, "movie.bin", &same);
}
