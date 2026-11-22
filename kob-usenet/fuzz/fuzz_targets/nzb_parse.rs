#![no_main]

use kob_usenet::ReleaseKey;
use kob_usenet::vendor::queue::nzb;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|raw: &[u8]| {
    let Ok(xml) = nzb::decompress(raw) else {
        return;
    };
    let Ok(parsed) = nzb::parse(&xml) else {
        return;
    };
    assert!(!parsed.files.is_empty());
    for file in &parsed.files {
        assert!(!file.segments.is_empty());
        let _ = (file.name(), file.bytes());
    }
    let _ = parsed.content_hash();
    let key = ReleaseKey::of(&parsed);
    assert_eq!(key.as_str().parse::<ReleaseKey>().ok(), Some(key));
});
