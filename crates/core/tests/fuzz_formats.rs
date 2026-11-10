//! Damaged input never panics: every native format reader gets thousands of
//! randomly damaged copies of real sample files (deterministic seeds).

use std::path::Path;
use yakusu_core::engines::livemaker::{self, lsb::Lsb, tpword::TpWord};
use yakusu_core::engines::rgss::{archive as rgss_archive, marshal};
use yakusu_core::engines::unity::serialized::SerializedFile;

fn fixture(rel: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(rel)).unwrap()
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Damaged copies: flipped bytes, truncations, and chunks cut out.
fn mutants(src: &[u8], n: usize, seed: u64) -> Vec<Vec<u8>> {
    let mut r = Lcg(seed);
    (0..n)
        .map(|_| {
            let mut v = src.to_vec();
            match r.next() % 3 {
                0 => {
                    for _ in 0..1 + r.next() % 8 {
                        let i = (r.next() as usize) % v.len().max(1);
                        if !v.is_empty() {
                            v[i] = r.next() as u8;
                        }
                    }
                }
                1 => {
                    let n = (r.next() as usize) % (v.len() + 1);
                    v.truncate(n);
                }
                _ => {
                    if v.len() > 2 {
                        let a = (r.next() as usize) % v.len();
                        let b = (a + 1 + (r.next() as usize) % 16).min(v.len());
                        v.drain(a..b);
                    }
                }
            }
            v
        })
        .collect()
}

#[test]
fn livemaker_scripts_never_panic() {
    for f in ["livemaker/00000001.lsb", "livemaker/gamemain.lsb"] {
        for m in mutants(&fixture(f), 3000, 7) {
            if let Ok(l) = Lsb::parse(m) {
                let es = livemaker::export(&l);
                let _ = livemaker::apply(&l, &es, true);
                for i in 0..l.texts.len() {
                    if let Ok(mut tw) = TpWord::parse(l.text(i)) {
                        for b in tw.blocks().into_iter().rev() {
                            let _ = tw.replace(&b, "x\ny <v1>");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn livemaker_archives_never_panic() {
    let tmp = tempfile::tempdir().unwrap();
    for f in ["livemaker/test.exe", "livemaker/test.dat", "livemaker/save.dat"] {
        for (k, m) in mutants(&fixture(f), 1500, 11).into_iter().enumerate() {
            let p = tmp.path().join(format!("m{k}.dat"));
            std::fs::write(&p, &m).unwrap();
            if let Ok(a) = livemaker::archive::Archive::open(&p) {
                for e in &a.entries {
                    let _ = a.read(e);
                }
            }
        }
    }
}

#[test]
fn unity_files_never_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let b = yakusu_core::engines::unity::bundle::Bundle::open(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unity/banner_1"),
    )
    .unwrap();
    let data = b.data().unwrap();
    let cab = b.node(&data, &b.nodes[0]).unwrap().to_vec();
    for m in mutants(&cab, 3000, 13) {
        if let Ok(sf) = SerializedFile::parse(&m) {
            for o in &sf.objects {
                let obj = sf.object(&m, o);
                let _ = sf.strings(obj, o);
                let _ = sf.text_asset(obj);
            }
            let _ = sf.rebuild(&m, &Default::default());
        }
    }
    for (k, m) in mutants(&fixture("unity/banner_1"), 600, 17).into_iter().enumerate() {
        let p = tmp.path().join(format!("b{k}"));
        std::fs::write(&p, &m).unwrap();
        if let Ok(b) = yakusu_core::engines::unity::bundle::Bundle::open(&p) {
            let _ = b.data();
            let _ = b.classes();
        }
    }
}

#[test]
fn rgss_never_panics() {
    for f in ["rgss/Map001.rvdata2", "rgss/System.rvdata2", "rgss/CommonEvents.rxdata"] {
        for m in mutants(&fixture(f), 3000, 19) {
            if let Ok((v, syms)) = marshal::load(&m) {
                let es = yakusu_core::engines::rgss::export(&v, &syms, false);
                if let Ok(nv) = yakusu_core::engines::rgss::apply(&v, &syms, &es, false) {
                    let _ = marshal::dump(&nv, &syms);
                }
            }
        }
    }
    let arc = rgss_archive::create(3, &[("Data/a.rvdata2", b"abc"), ("Data/b", b"defgh")]);
    let arc1 = rgss_archive::create(1, &[("Data/a.rvdata2", b"abc"), ("Data/b", b"defgh")]);
    for src in [arc, arc1] {
        for m in mutants(&src, 2000, 23) {
            if let Ok(a) = rgss_archive::Archive::parse(m) {
                for e in &a.entries {
                    let _ = a.read(e);
                }
                let _ = a.rebuild(&Default::default());
            }
        }
    }
}
