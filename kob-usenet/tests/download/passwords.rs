use crate::compressed::FIXTURE_FILES;
use crate::nzb::{self, nzb_with};
use crate::post::Posted;
use crate::support::{Rig, completed, crc_of, extractor_on_path, failed, finish, unrar_on_path};

const FILES_ONLY: &[u8] = include_bytes!("../vendor/fixtures/archive/Rar5.encrypted_filesOnly.rar");
const RAR4_FILES_ONLY: &[u8] =
    include_bytes!("../vendor/fixtures/archive/Rar.encrypted_filesOnly.rar");
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
        (Some("wrong"), Some("test")),
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

#[tokio::test]
async fn the_stored_job_spec_holding_the_password_is_private_to_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let rar = posted(FILES_ONLY);
    let rig = Rig::new(rar.articles.clone()).await;
    let engine = rig.engine().await;
    engine
        .add_job(rig.spec("Locked", nzb::of(&[&rar]), Some("test")))
        .await
        .unwrap();

    let mode = std::fs::metadata(rig.record("spec"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
}

#[tokio::test]
async fn a_damaged_encrypted_archive_unlocked_by_the_right_password_is_not_called_a_wrong_password()
{
    if !unrar_on_path() {
        return;
    }
    let mut damaged = RAR4_FILES_ONLY.to_vec();
    let middle = damaged.len() / 2;
    for byte in &mut damaged[middle..middle + 40] {
        *byte ^= 0x55;
    }
    let rar = posted(&damaged);
    let rig = Rig::new(rar.articles.clone()).await;
    let engine = rig
        .engine_with(|c| c.download.sevenzip = "/nonexistent/7z".into())
        .await;
    let spec = rig.spec("Locked", nzb::of(&[&rar]), Some("test"));
    let id = spec.id.clone();
    engine.add_job(spec).await.unwrap();

    let error = failed(&finish(&engine, &id).await);
    assert!(!error.contains("Wrong archive password"), "{error}");
    assert!(error.contains("damaged"), "{error}");
}
