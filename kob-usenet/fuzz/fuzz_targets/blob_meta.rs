#![no_main]

use kob_usenet::vendor::core::blob;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(meta) = blob::decode_meta_limited(data, 1 << 24) else {
        return;
    };
    let _ = meta.size();
    let encoded = blob::encode_meta(&meta).unwrap();
    assert_eq!(blob::decode_meta(&encoded).unwrap(), meta);
});
