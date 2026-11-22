use std::fmt::Write as _;

use kob_usenet::vendor::testkit::Article;

use crate::post::Posted;

pub const POSTED: i64 = 1_790_000_000;

pub struct Listed<'a> {
    pub subject: String,
    pub segments: Vec<(usize, &'a Article)>,
}

impl Posted {
    pub fn listed(&self) -> Listed<'_> {
        self.listed_as(&self.name)
    }

    pub fn listed_as(&self, name: &str) -> Listed<'_> {
        Listed {
            subject: format!("\"{name}\" yEnc (1/{})", self.articles.len()),
            segments: self
                .articles
                .iter()
                .enumerate()
                .map(|(i, a)| (i + 1, a))
                .collect(),
        }
    }
}

pub fn nzb(files: &[Listed<'_>]) -> Vec<u8> {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n",
    );
    for (n, file) in files.iter().enumerate() {
        let subject =
            format!("[{}/{}] - {}", n + 1, files.len(), file.subject).replace('"', "&quot;");
        let _ = writeln!(
            xml,
            "<file poster=\"p\" date=\"{POSTED}\" subject=\"{subject}\"><groups><group>a.b.test</group></groups><segments>"
        );
        for (number, article) in &file.segments {
            let _ = writeln!(
                xml,
                "<segment bytes=\"{}\" number=\"{number}\">{}</segment>",
                article.wire.len(),
                article.message_id
            );
        }
        xml.push_str("</segments></file>\n");
    }
    xml.push_str("</nzb>\n");
    xml.into_bytes()
}

pub fn of(posts: &[&Posted]) -> Vec<u8> {
    nzb(&posts.iter().map(|p| p.listed()).collect::<Vec<_>>())
}
