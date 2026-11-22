#![allow(clippy::unwrap_used, clippy::expect_used)]

mod clean;
mod compressed;
mod durable;
mod firsts;
mod hedge;
mod hopeless;
mod nested;
#[allow(dead_code)]
#[path = "../common/nzb.rs"]
mod nzb;
mod obfuscated;
mod outage;
#[allow(dead_code)]
#[path = "../common/par2.rs"]
mod par2;
mod passwords;
#[allow(dead_code)]
#[path = "../common/post.rs"]
mod post;
#[allow(dead_code)]
#[path = "../common/rar.rs"]
mod rar;
mod recovery;
mod remove;
mod repair;
mod restart;
mod resume;
mod space;
mod staging;
mod support;
mod unpack;
