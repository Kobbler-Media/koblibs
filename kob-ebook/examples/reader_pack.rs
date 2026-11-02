//! Build a reader pack from an EPUB and write it out as the server stores it:
//! `manifest.json`, `chapters/N.json` and `sidecars.json`.
//!
//! `cargo run --release --example reader_pack -- book.epub out/`

use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let (Some(epub), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: reader_pack <book.epub> <out-dir>");
        std::process::exit(2);
    };
    let (epub, out) = (PathBuf::from(epub), PathBuf::from(out));
    let bytes = std::fs::read(&epub)?;
    let started = Instant::now();
    let pack = kob_ebook::reader_pack::build(&bytes)?;
    let elapsed = started.elapsed();

    std::fs::create_dir_all(out.join("chapters"))?;
    std::fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&pack.manifest)?,
    )?;
    for (i, chapter) in pack.chapters.iter().enumerate() {
        std::fs::write(
            out.join(format!("chapters/{i}.json")),
            serde_json::to_vec(chapter)?,
        )?;
    }
    std::fs::write(
        out.join("sidecars.json"),
        serde_json::to_vec(&pack.sidecars)?,
    )?;

    let blocks: usize = pack.chapters.iter().map(|c| c.blocks.len()).sum();
    println!(
        "{}: {} chapters, {} blocks, {} styles, built in {:.1} ms",
        epub.display(),
        pack.chapters.len(),
        blocks,
        pack.manifest.styles.len(),
        elapsed.as_secs_f64() * 1000.0
    );
    Ok(())
}
