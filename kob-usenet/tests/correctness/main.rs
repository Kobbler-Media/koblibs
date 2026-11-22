#![allow(clippy::unwrap_used, clippy::expect_used)]

mod archives;
mod benchmark;
mod damage;
mod encrypted;
mod geometry;
mod harness;
#[allow(dead_code)]
#[path = "../common/nzb.rs"]
mod nzb;
mod obfuscated;
mod packs;
#[allow(dead_code)]
#[path = "../common/par2.rs"]
mod par2;
#[allow(dead_code)]
#[path = "../common/post.rs"]
mod post;
mod proxy;
#[allow(dead_code)]
#[path = "../common/rar.rs"]
mod rar;
mod tiers;
mod warming;
