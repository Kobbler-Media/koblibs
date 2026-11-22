use kob_usenet::vendor::queue::import::Error as ImportError;
use kob_usenet::{Error, ImportMode};

use crate::harness::{Rig, served};
use crate::nzb;
use crate::post::Posted;

// `7z a -mx0 -ptest` of `film()`: AES data, plain headers, so no password check at import.
const FILM_7Z: &[u8] = include_bytes!("../fixtures/Film.2026.7z");
const AES_RAR4: &[u8] = include_bytes!("../vendor/fixtures/archive/aes-4.rar");

fn film() -> Vec<u8> {
    let mut data: Vec<u8> = (0..40_000u32)
        .map(|i| ((i * 7 + i / 251) & 0xff) as u8)
        .collect();
    data[..4].copy_from_slice(&[0x1A, 0x45, 0xDF, 0xA3]);
    data
}

async fn import(
    archive: &[u8],
    name: &str,
    password: &str,
) -> (Rig, Result<kob_usenet::Release, Error>) {
    let post = Posted::new("enc", name, archive.to_vec());
    let rig = Rig::new(vec![post.articles.clone()]).await;
    let release = rig
        .engine
        .import(
            Some(&nzb::of(&[&post])),
            None,
            "Film.2026",
            Some(password),
            ImportMode::Stream,
        )
        .await;
    (rig, release)
}

#[tokio::test]
async fn an_encrypted_member_streams_only_when_its_password_decrypts_it() {
    let (rig, release) = import(FILM_7Z, "Film.2026.7z", "test").await;
    let release = release.unwrap();
    let name = release.outputs[0]
        .path
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    served(&rig.engine, &release, &name, &film()).await;
    let mut reader = rig
        .engine
        .open(
            &release.key,
            release.outputs[0].index,
            kob_usenet::Purpose::Play,
        )
        .unwrap();
    reader.set_end(1000);
    let mut head = vec![0; 1000];
    tokio::io::AsyncReadExt::read_exact(&mut reader, &mut head)
        .await
        .unwrap();
    assert!(head == film()[..1000]);

    let (_, wrong) = import(FILM_7Z, "Film.2026.7z", "nope").await;
    let error = wrong
        .expect_err("a wrong password must not stream")
        .to_string();
    assert!(error.contains("password"), "{error}");
}

#[tokio::test]
async fn an_encrypted_member_whose_password_cannot_be_checked_is_not_streamed() {
    for password in ["test", "nope"] {
        let (_, release) = import(AES_RAR4, "Data.rar", password).await;
        assert!(
            matches!(
                release,
                Err(Error::Import(
                    ImportError::Unsupported(_) | ImportError::Password(_)
                ))
            ),
            "{password}: {release:?}"
        );
    }
}
