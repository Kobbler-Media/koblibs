use kob_usenet::vendor::testkit::Article;

use crate::harness::{Rig, imported, read_whole, refused_or_absent, served};
use crate::nzb;
use crate::par2;
use crate::post::{ARTICLE, Posted, articles, noise, video};
use crate::rar::{self, Member};

const SIZES: [usize; 6] = [ARTICLE, ARTICLE, ARTICLE - 100, ARTICLE, ARTICLE, 777];

#[derive(Clone, Copy, Debug)]
enum Swap {
    FirstPart,
    Neighbour,
    OtherFile,
    Missing,
}

fn uneven(prefix: &str, name: &str, seed: u64) -> Posted {
    Posted::uneven(prefix, name, video(SIZES.iter().sum(), seed), &SIZES)
}

fn other_file() -> Posted {
    let mut sizes = SIZES.to_vec();
    sizes.push(4321);
    Posted::uneven(
        "o",
        "Uneven.sample.mkv",
        noise(sizes.iter().sum(), 6),
        &sizes,
    )
}

fn swapped(film: &Posted, part: usize, swap: Swap) -> Vec<Article> {
    let mut articles = film.articles.clone();
    let wire = match swap {
        Swap::FirstPart => film.articles[0].wire.clone(),
        Swap::Neighbour => film.articles[part - 1].wire.clone(),
        Swap::OtherFile => other_file().articles[part].wire.clone(),
        Swap::Missing => {
            articles.remove(part);
            return articles;
        }
    };
    articles[part].wire = wire;
    articles
}

async fn case(part: usize, swap: Swap, everywhere: bool) {
    let film = uneven("u", "Uneven.mkv", 5);
    let bad = swapped(&film, part, swap);
    let good = if everywhere {
        bad.clone()
    } else {
        film.articles.clone()
    };
    let rig = Rig::new(vec![bad, good]).await;
    let release = rig.import(&nzb::of(&[&film])).await;
    if everywhere {
        let Some(release) = imported(release, &format!("{swap:?} part {part}")) else {
            return;
        };
        refused_or_absent(&rig.engine, &release, &film.name, &film.data).await;
    } else {
        let release =
            release.unwrap_or_else(|e| panic!("{swap:?} part {part}: import failed: {e}"));
        served(&rig.engine, &release, &film.name, &film.data).await;
    }
}

macro_rules! cases {
    ($($name:ident: $swap:ident, $part:expr, $everywhere:expr;)*) => {
        $(
            #[tokio::test]
            async fn $name() {
                case($part, Swap::$swap, $everywhere).await;
            }
        )*
    };
}

cases! {
    measured_first_part_under_a_middle_id_on_primary_is_served_from_backup: FirstPart, 3, false;
    measured_neighbour_under_a_middle_id_on_primary_is_served_from_backup: Neighbour, 3, false;
    measured_other_file_middle_on_primary_is_served_from_backup: OtherFile, 3, false;
    measured_other_file_last_on_primary_is_served_from_backup: OtherFile, 5, false;
    measured_missing_last_on_primary_is_served_from_backup: Missing, 5, false;
    measured_first_part_under_a_middle_id_everywhere_is_refused: FirstPart, 3, true;
    measured_other_file_middle_everywhere_is_refused: OtherFile, 3, true;
    measured_other_file_last_everywhere_is_refused: OtherFile, 5, true;
    measured_missing_last_everywhere_is_refused: Missing, 5, true;
}

#[tokio::test]
async fn a_volume_whose_posted_size_is_wrong_still_streams_its_member() {
    let film = video(3 * ARTICLE + 4321, 9);
    let set = rar::stored(
        &[Member {
            name: "Lie.mkv",
            data: &film,
        }],
        100 * ARTICLE,
    );
    let volume = set.volumes[0].clone();
    let claimed = volume.len() - 300;
    let posted = Posted::claiming("lv", "Lie.rar", volume, claimed);
    let rig = Rig::new(vec![posted.articles.clone(), posted.articles.clone()]).await;
    let release = rig.import(&nzb::of(&[&posted])).await.unwrap();
    let index = release
        .outputs
        .iter()
        .find(|o| o.path.ends_with(".mkv"))
        .unwrap_or_else(|| panic!("{:?}", release.outputs))
        .index;
    read_whole(&rig.engine, &release.key, index, &film)
        .await
        .unwrap();
    assert_eq!(rig.servers[1].body_commands(), 0);
}

#[tokio::test]
async fn a_loose_file_whose_posted_size_its_par2_disproves_still_streams() {
    let film = video(5 * ARTICLE + 4321, 10);
    let posted = Posted::claiming("lf", "Lie.mkv", film.clone(), film.len() - 300);
    let par2 = Posted::new("lp", "Lie.par2", par2::index(&[("Lie.mkv", &film)]));
    let posts = [&posted, &par2];
    let healthy = articles(posts);
    let rig = Rig::new(vec![healthy.clone(), healthy]).await;
    let release = rig.import(&nzb::of(&posts)).await.unwrap();
    served(&rig.engine, &release, "Show.S01.mkv", &film).await;
    assert_eq!(rig.servers[1].body_commands(), 0);
}
