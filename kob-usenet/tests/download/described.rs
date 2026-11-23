use kob_usenet::JobState;

use crate::obfuscated::hash;
use crate::post::{ARTICLE, Posted, noise};
use crate::support::{Rig, assert_file, completed, listing, par2_set, run_job};

const SLICE: usize = ARTICLE / 2 / 4 * 4;

#[tokio::test]
async fn a_first_article_serving_another_files_part_one_is_not_a_second_copy() {
    for swapped in [0, 1] {
        let one = Posted::new("one", "One.bin", noise(4 * ARTICLE, 11));
        let two = Posted::new("two", "Two.bin", noise(4 * ARTICLE, 12));
        let rar = Posted::new(
            "rar",
            "Release.rar",
            include_bytes!("../vendor/fixtures/archive/Rar.rar").to_vec(),
        );
        let sets = par2_set(&[&one, &two, &rar], "S", SLICE, 100);
        let mut posts = vec![&one, &two, &rar];
        posts.extend(&sets);
        let mut articles: Vec<_> = posts
            .iter()
            .flat_map(|p| p.articles.iter().cloned())
            .collect();
        let (victim, donor) = if swapped == 0 {
            (&two, &one)
        } else {
            (&one, &two)
        };
        let wire = donor.articles[0].wire.clone();
        articles
            .iter_mut()
            .find(|a| a.message_id == victim.id(0))
            .unwrap()
            .wire = wire;
        let rig = Rig::new(articles).await;
        let engine = rig.engine().await;
        let dir = completed(&run_job(&rig, &engine, "Swap", &posts).await);
        assert_file(&dir, &one.name, &one.data);
        assert_file(&dir, &two.name, &two.data);
        assert!(
            !listing(&dir).iter().any(|n| n.ends_with(".1")),
            "{:?}",
            listing(&dir)
        );
    }
}

#[tokio::test]
async fn with_obfuscated_names_a_swapped_first_article_never_completes_wrong_bytes() {
    for swapped in [0, 1] {
        let one = Posted::new("real1", "One.bin", noise(4 * ARTICLE, 21));
        let two = Posted::new("real2", "Two.bin", noise(4 * ARTICLE, 22));
        let sets = par2_set(&[&one, &two], "S", SLICE, 100);
        let hidden = [
            Posted::new("one", &hash("One.bin"), one.data.clone()),
            Posted::new("two", &hash("Two.bin"), two.data.clone()),
        ];
        let mut posts: Vec<&Posted> = hidden.iter().collect();
        posts.extend(&sets);
        let mut articles: Vec<_> = posts
            .iter()
            .flat_map(|p| p.articles.iter().cloned())
            .collect();
        let wire = hidden[swapped].articles[0].wire.clone();
        articles
            .iter_mut()
            .find(|a| a.message_id == hidden[1 - swapped].id(0))
            .unwrap()
            .wire = wire;
        let rig = Rig::new(articles).await;
        let engine = rig.engine().await;
        let status = run_job(&rig, &engine, "Swap", &posts).await;
        if status.state == JobState::Completed {
            assert_file(&status.path, &one.name, &one.data);
            assert_file(&status.path, &two.name, &two.data);
        } else {
            assert_eq!(status.state, JobState::Failed, "{status:?}");
        }
    }
}
