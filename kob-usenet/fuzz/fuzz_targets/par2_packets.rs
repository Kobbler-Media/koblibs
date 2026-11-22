#![no_main]

use kob_usenet::vendor::par2::{self, Reader};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let index = par2::read(data);
    for file in &index.files {
        let _ = (&file.name, file.length, file.slices.len());
    }
    let (head, tail) = data.split_at(data.len() / 2);
    let mut reader = Reader::default();
    reader.push(head);
    reader.end_volume();
    let _ = reader.push_bounded(tail, 1 << 20);
    for set in reader.sets() {
        let _ = reader.repair_set(set).map(|s| s.articles(4096, &[0, 1]));
        let _ = reader.verification_set(set);
        let _ = reader.recovery_slices(set, 4096);
    }
    let _ = reader.index();
});
