#![allow(clippy::unwrap_used, clippy::expect_used)]

mod clean;
mod compressed;
mod hedge;
mod hopeless;
#[allow(dead_code)]
#[path = "../common/nzb.rs"]
mod nzb;
mod obfuscated;
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
mod remove;
mod repair;
mod restart;
mod space;
mod support;
