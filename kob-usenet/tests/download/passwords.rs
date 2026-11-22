use crate::compressed::FIXTURE_FILES;
use crate::nzb::{self, nzb_with};
use crate::post::Posted;
use crate::support::{Rig, completed, crc_of, extractor_on_path, failed, finish};

const FILES_ONLY: &[u8] = include_bytes!("../vendor/fixtures/archive/Rar5.encrypted_filesOnly.rar");
const HEADERS_TOO: &[u8] =
    include_bytes!("../vendor/fixtures/archive/Rar5.encrypted_filesAndHeader.rar");

fn posted(archive: &[u8]) -> Posted {
    Posted::new("rar", "Locked.rar", archive.to_vec())
}

#[tokio::test]
async fn a_password_protected_archive_without_a_password_fails_with_a_clear_error() {
    if !extractor_on_path() {
        return;
    }
    for archive in [FILES_ONLY, HEADERS_TOO] {
        let rar = posted(archive);
        let rig = Rig::new(rar.articles.clone()).await;
        let engine = rig.engine().await;
        let spec = rig.spec("Locked", nzb::of(&[&rar]), None);
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();

        let error = failed(&finish(&engine, &id).await);
        assert!(
            error.contains("password-protected") && error.contains("no password"),
            "{error}"
        );
        assert!(!rig.dest.path().join("Locked").exists());
    }
}

#[tokio::test]
async fn the_nzb_password_then_the_job_password_unlock_the_archive_for_the_external_tool() {
    if !extractor_on_path() {
        return;
    }
    for (meta, job) in [
        (Some("test"), None),
        (None, Some("test")),
        (Some("test"), Some("wrong")),
    ] {
        let rar = posted(FILES_ONLY);
        let rig = Rig::new(rar.articles.clone()).await;
        let engine = rig.engine().await;
        let meta: Vec<(&str, &str)> = meta.map(|pw| ("password", pw)).into_iter().collect();
        let spec = rig.spec("Locked", nzb_with(&meta, &[rar.listed()]), job);
        let id = spec.id.clone();
        engine.add_job(spec).await.unwrap();

        let dir = completed(&finish(&engine, &id).await);
        for (name, len, crc) in FIXTURE_FILES {
            assert_eq!(
                crc_of(&dir, name),
                (len, crc),
                "{name} with {meta:?} / {job:?}"
            );
        }
    }
}
