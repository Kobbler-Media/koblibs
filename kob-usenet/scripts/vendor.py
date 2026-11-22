import os, re, shutil, sys
from pathlib import Path

UP = Path(sys.argv[1])
SF = Path(sys.argv[2]) if len(sys.argv) > 2 and sys.argv[2] != "-" else None
DST = Path(sys.argv[3])
src_root = UP / "crates"

MODS = {
    "core": ("core", None),
    "yenc": ("yenc", None),
    "nntp": ("nntp", None),
    "stream": ("stream", None),
    "archive": ("archive", None),
    "par2": ("par2", None),
    "testkit": ("testkit", None),
}
CRATE_MAP = {
    "infinidysk_core": "crate::vendor::core",
    "infinidysk_yenc": "crate::vendor::yenc",
    "infinidysk_nntp": "crate::vendor::nntp",
    "infinidysk_stream": "crate::vendor::stream",
    "infinidysk_archive": "crate::vendor::archive",
    "infinidysk_par2": "crate::vendor::par2",
    "infinidysk_testkit": "crate::vendor::testkit",
    "infinidysk_queue": "crate::vendor::queue",
    "infinidysk_vfs": "crate::vendor::vfs",
    "infinidysk_db": "crate::vendor::stubs",
}
TEST_MAP = {k: v.replace("crate::", "kob_usenet::") for k, v in CRATE_MAP.items()}


def rewrite(text, mod, crate_map):
    text = re.sub(r"(?<![\w$])crate::", f"crate::vendor::{mod}::", text) if mod else text
    text = re.sub(r"\$crate::", f"$crate::vendor::{mod}::", text) if mod else text
    for k, v in crate_map.items():
        text = re.sub(rf"\b{k}\b", v, text)
    return text


def copy_tree(src, dst, mod):
    for p in sorted(src.rglob("*.rs")):
        rel = p.relative_to(src)
        if mod == "nntp" and rel.name == "catalog.rs":
            continue
        out = dst / rel
        if rel == Path("lib.rs"):
            out = dst / "mod.rs"
        out.parent.mkdir(parents=True, exist_ok=True)
        t = p.read_text()
        if mod == "nntp" and rel == Path("patch/tests.rs"):
            t = t.replace('"patch::tests::crash_child"', '"vendor::nntp::patch::tests::crash_child"')
        if mod == "nntp" and rel == Path("lib.rs"):
            t = t.replace("pub mod catalog;\n", "")
        out.write_text(rewrite(t, mod, CRATE_MAP))


vendor = DST / "src" / "vendor"
if vendor.exists():
    shutil.rmtree(vendor)
vendor.mkdir(parents=True)
for m in MODS:
    copy_tree(src_root / m / "src", vendor / m, m)

q = vendor / "queue"
q.mkdir()
for f in ["nzb.rs", "names.rs", "import.rs", "import/inspect.rs"]:
    out = q / f
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(rewrite((src_root / "queue/src" / f).read_text(), "queue", CRATE_MAP))
(q / "mod.rs").write_text("pub mod import;\npub mod names;\npub mod nzb;\n")
(vendor / "stubs.rs").write_text("#[derive(Clone, Debug, PartialEq, Eq)]\npub struct FirstCheck;\n")

v = vendor / "vfs"
v.mkdir()
v_reader = (src_root / "vfs/src/reader.rs").read_text()
(v / "reader.rs").write_text(rewrite(v_reader, "vfs", CRATE_MAP).replace("    pub(crate) fn new(", "    pub fn new(", 1))
(v / "mod.rs").write_text("""pub mod reader;

pub use reader::Reader;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("stored blob is unreadable")]
    Blob(#[from] crate::vendor::core::blob::Error),
    #[error("stored data is invalid: {0}")]
    Corrupt(&'static str),
}
""")

tests = DST / "tests" / "vendor"
if tests.exists():
    shutil.rmtree(tests)
tests.mkdir(parents=True)
for m in ["nntp", "stream", "archive", "par2", "queue"]:
    tdir = src_root / m / "tests"
    for p in sorted(tdir.glob("*.rs")):
        if (m, p.name) == ("queue", "import.rs"):
            continue
        out = tests / f"{m}_{p.name}"
        t = rewrite(p.read_text(), None, TEST_MAP)
        t = t.replace('include_bytes!("fixtures/', f'include_bytes!("fixtures/{m}/')
        t = t.replace('"{}/tests/fixtures/{name}"', '"{}/tests/vendor/fixtures/%s/{name}"' % m)
        t = t.replace('"{}/../{path}"', '"{}/tests/vendor/{path}"')
        for c in ["archive", "par2", "queue"]:
            t = t.replace(f'"{c}/tests/fixtures/', f'"fixtures/{c}/')
        out.write_text(t)
    fx = tdir / "fixtures"
    if fx.exists():
        shutil.copytree(fx, tests / "fixtures" / m)

yd = DST / "rapidyenc"
if yd.exists():
    shutil.rmtree(yd)
shutil.copytree(src_root / "yenc/rapidyenc", yd, ignore=shutil.ignore_patterns(".git", "tool"))
shutil.copy(UP / "LICENSE", DST / "src/vendor/LICENSE-infinidysk")
(vendor / "mod.rs").write_text("""pub mod archive;
pub mod core;
pub mod nntp;
pub mod par2;
pub mod queue;
pub mod stream;
pub mod stubs;
#[cfg(feature = "testkit")]
pub mod testkit;
pub mod vfs;
pub mod yenc;
""")
print("ok")
mods = sorted(p.stem for p in tests.glob("*.rs"))
(tests / "main.rs").write_text("".join(f"mod {m};\n" for m in mods if m != "main"))
import subprocess
generated = sorted(str(p) for d in (vendor, tests) for p in d.rglob("*.rs"))
subprocess.run(["rustfmt", "--edition", "2024", *generated], check=True)
