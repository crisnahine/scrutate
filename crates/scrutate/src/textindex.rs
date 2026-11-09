//! Engine B text index: trigram postings over exactly the files Engine A's default walk searches,
//! so `grep --index` reads only the files that can contain a match. Every candidate file is still
//! searched by Engine A's matcher, so the output equals Engine A's output on the indexed snapshot.
//!
//! Layout under `.scrutate/text/`: `manifest.json` names the live segments; a segment directory
//! holds `keys.npy` (sorted u32 trigrams), `ptr.npy` (u64 offsets into `post.bin`), `post.bin`
//! (delta + LEB128 document numbers), `docs.npy` (size, mtime, flags per document), `paths.bin` +
//! `poff.npy` (paths), and `live-<generation>.bin` (which documents are current). Segments are
//! immutable; an update writes new segments and new liveness files, then swaps the manifest.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use memmap2::Mmap;
use rayon::prelude::*;
use regex_syntax::hir::{Class, Hir, HirKind};

use crate::instant::{self, Case, Mode, Options};
use crate::npy::{self, Npy};

pub const FORMAT: u32 = 1;
const MAX_SEGMENTS: usize = 4;
/// Postings held in memory before a batch is written as a segment (about 200 MB at the peak).
const BATCH_POSTINGS: usize = 16 << 20;
const MAX_SET: usize = 64;
const MAX_CLASS: u32 = 16;
/// The file has a NUL byte. Line-mode search stops at the first NUL, so trigrams cover only the
/// bytes before it; multi-line search may read the whole file, so such files are always searched.
pub const BINARY: i64 = 1;
/// UTF-16 (Engine A transcodes it) or unreadable when indexed: always searched, Engine A decides.
pub const ALWAYS: i64 = 2;
/// Unreadable when indexed: read again by the next update.
pub const RETRY: i64 = 4;

// ------------------------------------------------------------------------- query extraction
/// A necessary condition on a file's trigram set for the pattern to match somewhere in it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Q {
    All,
    Tri(u32),
    And(Vec<Q>),
    Or(Vec<Q>),
}

fn and(qs: Vec<Q>) -> Q {
    let mut out = Vec::new();
    for q in qs {
        match q {
            Q::All => {}
            Q::And(v) => out.extend(v),
            q => out.push(q),
        }
    }
    out.sort();
    out.dedup();
    match out.len() {
        0 => Q::All,
        1 => out.pop().unwrap_or(Q::All),
        _ => Q::And(out),
    }
}

fn or(qs: Vec<Q>) -> Q {
    let mut out = Vec::new();
    for q in qs {
        match q {
            Q::All => return Q::All,
            Q::Or(v) => out.extend(v),
            q => out.push(q),
        }
    }
    out.sort();
    out.dedup();
    match out.len() {
        0 => Q::All,
        1 => out.pop().unwrap_or(Q::All),
        _ => Q::Or(out),
    }
}

/// The index folds ASCII case, so queries are built from folded strings.
fn fold(s: &[u8]) -> Vec<u8> {
    s.to_ascii_lowercase()
}

fn string_query(s: &[u8]) -> Q {
    if s.len() < 3 {
        return Q::All;
    }
    and(s.windows(3).map(|w| Q::Tri(key(w[0], w[1], w[2]))).collect())
}

fn set_query(set: &[Vec<u8>]) -> Q {
    or(set.iter().map(|s| string_query(s)).collect())
}

fn key(a: u8, b: u8, c: u8) -> u32 {
    (a.to_ascii_lowercase() as u32) << 16 | (b.to_ascii_lowercase() as u32) << 8 | c.to_ascii_lowercase() as u32
}

/// What is known about the strings a sub-pattern matches: all of them (`exact`, folded, small
/// sets only) or a condition every match satisfies.
struct Info {
    exact: Option<Vec<Vec<u8>>>,
    q: Q,
}

impl Info {
    fn query(&self) -> Q {
        match &self.exact {
            Some(s) => set_query(s),
            None => self.q.clone(),
        }
    }
}

fn exact(mut v: Vec<Vec<u8>>) -> Info {
    v.sort();
    v.dedup();
    Info { exact: Some(v), q: Q::All }
}

fn inexact(q: Q) -> Info {
    Info { exact: None, q }
}

fn cross(a: &[Vec<u8>], b: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
    if a.len() * b.len() > MAX_SET {
        return None;
    }
    let mut out = Vec::with_capacity(a.len() * b.len());
    for x in a {
        for y in b {
            let mut s = x.clone();
            s.extend_from_slice(y);
            out.push(s);
        }
    }
    out.sort();
    out.dedup();
    Some(out)
}

fn class_strings(c: &Class) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    match c {
        Class::Unicode(u) => {
            let n: u32 = u.ranges().iter().map(|r| r.end() as u32 - r.start() as u32 + 1).sum();
            if n > MAX_CLASS {
                return None;
            }
            for r in u.ranges() {
                for cp in r.start() as u32..=r.end() as u32 {
                    if let Some(ch) = char::from_u32(cp) {
                        out.push(fold(ch.encode_utf8(&mut [0u8; 4]).as_bytes()));
                    }
                }
            }
        }
        Class::Bytes(b) => {
            let n: u32 = b.ranges().iter().map(|r| r.end() as u32 - r.start() as u32 + 1).sum();
            if n > MAX_CLASS {
                return None;
            }
            for r in b.ranges() {
                for x in r.start()..=r.end() {
                    out.push(vec![x.to_ascii_lowercase()]);
                }
            }
        }
    }
    Some(out)
}

fn analyze(h: &Hir) -> Info {
    match h.kind() {
        HirKind::Empty | HirKind::Look(_) => exact(vec![vec![]]),
        HirKind::Literal(l) => exact(vec![fold(&l.0)]),
        HirKind::Class(c) => match class_strings(c) {
            Some(v) if !v.is_empty() => exact(v),
            _ => inexact(Q::All),
        },
        HirKind::Capture(c) => analyze(&c.sub),
        HirKind::Repetition(r) => {
            let sub = analyze(&r.sub);
            if r.min == 0 {
                if r.max == Some(1) {
                    if let Some(mut e) = sub.exact {
                        e.push(Vec::new());
                        if e.len() <= MAX_SET {
                            return exact(e);
                        }
                    }
                }
                return inexact(Q::All);
            }
            if let (Some(e), Some(max)) = (&sub.exact, r.max) {
                if max == r.min && r.min <= 4 {
                    let mut acc = vec![Vec::new()];
                    for _ in 0..r.min {
                        match cross(&acc, e) {
                            Some(x) => acc = x,
                            None => return inexact(sub.query()),
                        }
                    }
                    return exact(acc);
                }
            }
            // every match contains at least one match of the repeated part
            inexact(sub.query())
        }
        HirKind::Concat(cs) => {
            let mut run: Vec<Vec<u8>> = vec![Vec::new()];
            let mut qs = Vec::new();
            let mut all_exact = true;
            for c in cs {
                let i = analyze(c);
                match i.exact {
                    Some(e) => match cross(&run, &e) {
                        Some(x) => run = x,
                        None => {
                            all_exact = false;
                            qs.push(set_query(&run));
                            run = e;
                        }
                    },
                    None => {
                        all_exact = false;
                        qs.push(set_query(&run));
                        qs.push(i.q);
                        run = vec![Vec::new()];
                    }
                }
            }
            if all_exact {
                exact(run)
            } else {
                qs.push(set_query(&run));
                inexact(and(qs))
            }
        }
        HirKind::Alternation(cs) => {
            let infos: Vec<Info> = cs.iter().map(analyze).collect();
            if infos.iter().all(|i| i.exact.is_some()) {
                let u: Vec<Vec<u8>> = infos.iter().flat_map(|i| i.exact.clone().unwrap_or_default()).collect();
                if u.len() <= MAX_SET {
                    return exact(u);
                }
            }
            inexact(or(infos.iter().map(Info::query).collect()))
        }
    }
}

/// The trigram condition for Engine A's options (same pattern translation and case rules as
/// `instant::build_matcher`). Inverted searches can match any file.
pub fn query_for(o: &Options) -> Result<Q> {
    if o.invert {
        return Ok(Q::All);
    }
    let insensitive = match o.case {
        Case::Sensitive => false,
        Case::Insensitive => true,
        Case::Smart => !o.pattern.chars().any(|c| c.is_uppercase()),
    };
    let pat = match o.mode {
        // k edits leave one of k+1 pieces verbatim (the fuzzy matcher's own prefilter)
        Mode::Fuzzy { max_edits } => {
            let p = o.pattern.as_bytes();
            let pieces = max_edits as usize + 1;
            let len = p.len() / pieces;
            if len < 3 {
                return Ok(Q::All);
            }
            let qs = (0..pieces).map(|i| string_query(&p[i * len..if i == pieces - 1 { p.len() } else { (i + 1) * len }])).collect();
            return Ok(or(qs));
        }
        Mode::Literal => regex::escape(&o.pattern),
        Mode::Identifier => instant::identifier_regex(&o.pattern),
        Mode::Regex => o.pattern.clone(),
        // PCRE2 syntax differs from this parser's in places: no condition, so no file is missed
        Mode::Pcre2 => return Ok(Q::All),
    };
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(insensitive && o.mode != Mode::Identifier)
        .multi_line(true)
        .utf8(false)
        .build()
        .parse(&pat)
        .with_context(|| format!("invalid pattern {:?}", o.pattern))?;
    Ok(analyze(&hir).query())
}

// ------------------------------------------------------------------------- trigram extraction
thread_local! {
    static SEEN: std::cell::RefCell<Vec<u64>> = std::cell::RefCell::new(vec![0u64; 1 << 18]);
}

/// Distinct folded trigrams of `bytes`, sorted.
pub fn trigrams(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    if bytes.len() < 3 {
        return out;
    }
    SEEN.with(|s| {
        let seen = &mut *s.borrow_mut();
        let mut k = (bytes[0].to_ascii_lowercase() as u32) << 8 | bytes[1].to_ascii_lowercase() as u32;
        for &b in &bytes[2..] {
            k = ((k << 8) | b.to_ascii_lowercase() as u32) & 0xFF_FFFF;
            let (w, bit) = ((k >> 6) as usize, 1u64 << (k & 63));
            if seen[w] & bit == 0 {
                seen[w] |= bit;
                out.push(k);
            }
        }
        for &k in &out {
            seen[(k >> 6) as usize] &= !(1u64 << (k & 63));
        }
    });
    out.sort_unstable();
    out
}

struct DocIn {
    rel: String,
    size: u64,
    mtime: i64,
    flags: i64,
    tris: Vec<u32>,
}

fn read_doc(root: &Path, rel: String, size: u64, mtime: i64) -> DocIn {
    let path = root.join(&rel);
    let data: Option<Box<dyn AsRef<[u8]>>> = if size > 64 << 20 {
        File::open(&path).ok().and_then(|f| unsafe { Mmap::map(&f) }.ok()).map(|m| Box::new(m) as Box<dyn AsRef<[u8]>>)
    } else {
        std::fs::read(&path).ok().map(|v| Box::new(v) as Box<dyn AsRef<[u8]>>)
    };
    let Some(data) = data else {
        return DocIn { rel, size, mtime, flags: ALWAYS | RETRY, tris: Vec::new() };
    };
    let bytes = (*data).as_ref();
    // Engine A transcodes UTF-16 files that start with a byte-order mark: search those directly
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return DocIn { rel, size, mtime, flags: ALWAYS, tris: Vec::new() };
    }
    let (text, flags) = match bytes.iter().position(|&b| b == 0) {
        Some(i) => (&bytes[..i], BINARY),
        None => (bytes, 0),
    };
    DocIn { rel, size, mtime, flags, tris: trigrams(text) }
}

// ------------------------------------------------------------------------- segments
fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Decode one posting list (document numbers below `ndocs`, strictly increasing).
fn decode_into(bytes: &[u8], ndocs: usize, mut each: impl FnMut(usize)) -> Result<()> {
    let mut i = 0;
    let mut prev = 0u64;
    let mut first = true;
    while i < bytes.len() {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = *bytes.get(i).ok_or_else(|| anyhow!("truncated posting list"))?;
            i += 1;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 42 {
                bail!("posting value too long");
            }
        }
        let doc = if first {
            first = false;
            v
        } else {
            if v == 0 {
                bail!("posting list not increasing");
            }
            prev + v
        };
        if doc as usize >= ndocs {
            bail!("posting {doc} outside the segment ({ndocs} documents)");
        }
        each(doc as usize);
        prev = doc;
    }
    Ok(())
}

fn map_file(path: &Path) -> Result<Option<Mmap>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    if f.metadata()?.len() == 0 {
        return Ok(None);
    }
    Ok(Some(unsafe { Mmap::map(&f) }.with_context(|| format!("mmap {}", path.display()))?))
}

struct Segment {
    name: String,
    keys: Npy,
    ptr: Npy,
    post: Option<Mmap>,
    docs: Npy,
    poff: Npy,
    paths: Option<Mmap>,
    live: Vec<u8>,
    ndocs: usize,
}

fn bytes(m: &Option<Mmap>) -> &[u8] {
    m.as_ref().map(|m| &m[..]).unwrap_or(&[])
}

impl Segment {
    fn open(dir: &Path, name: &str, live: &str) -> Result<Segment> {
        let d = dir.join(name);
        let keys = Npy::open(&d.join("keys.npy"))?;
        let ptr = Npy::open(&d.join("ptr.npy"))?;
        let docs = Npy::open(&d.join("docs.npy"))?;
        let poff = Npy::open(&d.join("poff.npy"))?;
        let post = map_file(&d.join("post.bin"))?;
        let paths = map_file(&d.join("paths.bin"))?;
        let live = std::fs::read(d.join(live)).with_context(|| format!("read {}/{live}", d.display()))?;
        let ndocs = docs.len() / 3;
        let s = Segment { name: name.to_string(), keys, ptr, post, docs, poff, paths, live, ndocs };
        s.validate(false).with_context(|| format!("text index segment {name}"))?;
        Ok(s)
    }

    fn keys(&self) -> &[u32] {
        self.keys.view().unwrap_or(&[])
    }
    fn ptr(&self) -> &[u64] {
        self.ptr.view().unwrap_or(&[])
    }
    fn docs(&self) -> &[i64] {
        self.docs.view().unwrap_or(&[])
    }
    fn poff(&self) -> &[u64] {
        self.poff.view().unwrap_or(&[])
    }

    /// Structural checks (cheap, on every open); `deep` also checks every key and posting.
    fn validate(&self, deep: bool) -> Result<()> {
        let keys: &[u32] = self.keys.view()?;
        let ptr: &[u64] = self.ptr.view()?;
        let docs: &[i64] = self.docs.view()?;
        let poff: &[u64] = self.poff.view()?;
        let post = bytes(&self.post);
        if !docs.len().is_multiple_of(3) || ptr.len() != keys.len() + 1 || poff.len() != self.ndocs + 1 {
            bail!("array lengths disagree");
        }
        if ptr[0] != 0 || ptr[keys.len()] as usize != post.len() || poff[0] != 0 || poff[self.ndocs] as usize != bytes(&self.paths).len() {
            bail!("offsets do not cover the data");
        }
        if self.live.len() != self.ndocs.div_ceil(8) {
            bail!("liveness bitmap has the wrong size");
        }
        if deep {
            if keys.windows(2).any(|w| w[0] >= w[1]) || ptr.windows(2).any(|w| w[0] > w[1]) || poff.windows(2).any(|w| w[0] > w[1]) {
                bail!("keys or offsets out of order");
            }
            for i in 0..keys.len() {
                decode_into(&post[ptr[i] as usize..ptr[i + 1] as usize], self.ndocs, |_| {})?;
            }
            for d in 0..self.ndocs {
                std::str::from_utf8(self.path_bytes(d)).context("path is not UTF-8")?;
            }
        }
        Ok(())
    }

    fn path_bytes(&self, d: usize) -> &[u8] {
        let poff = self.poff();
        bytes(&self.paths).get(poff[d] as usize..poff[d + 1] as usize).unwrap_or(&[])
    }
    fn path(&self, d: usize) -> &str {
        std::str::from_utf8(self.path_bytes(d)).unwrap_or("")
    }
    fn is_live(&self, d: usize) -> bool {
        self.live[d / 8] & (1 << (d % 8)) != 0
    }
    fn flags(&self, d: usize) -> i64 {
        self.docs()[d * 3 + 2]
    }

    fn posting(&self, t: u32) -> Option<&[u8]> {
        let i = self.keys().binary_search(&t).ok()?;
        let p = self.ptr();
        bytes(&self.post).get(p[i] as usize..p[i + 1] as usize)
    }

    fn eval(&self, q: &Q) -> Result<Option<Vec<u64>>> {
        let words = self.ndocs.div_ceil(64);
        Ok(match q {
            Q::All => None,
            Q::Tri(t) => {
                let mut bits = vec![0u64; words];
                if let Some(p) = self.posting(*t) {
                    decode_into(p, self.ndocs, |d| bits[d / 64] |= 1 << (d % 64))?;
                }
                Some(bits)
            }
            Q::And(qs) => {
                // rarest trigrams first: the intersection shrinks fastest and can stop early
                let mut order: Vec<&Q> = qs.iter().collect();
                order.sort_by_key(|q| match q {
                    Q::Tri(t) => self.posting(*t).map(|p| p.len()).unwrap_or(0),
                    _ => usize::MAX,
                });
                let mut acc: Option<Vec<u64>> = None;
                for q in order {
                    if let Some(b) = self.eval(q)? {
                        match &mut acc {
                            None => acc = Some(b),
                            Some(a) => a.iter_mut().zip(&b).for_each(|(x, y)| *x &= y),
                        }
                        if acc.as_ref().map(|a| a.iter().all(|&w| w == 0)).unwrap_or(false) {
                            break;
                        }
                    }
                }
                acc
            }
            Q::Or(qs) => {
                let mut acc = vec![0u64; words];
                for q in qs {
                    match self.eval(q)? {
                        None => return Ok(None),
                        Some(b) => acc.iter_mut().zip(&b).for_each(|(x, y)| *x |= y),
                    }
                }
                Some(acc)
            }
        })
    }
}

fn all_live(n: usize) -> Vec<u8> {
    let mut v = vec![0xFFu8; n.div_ceil(8)];
    if !n.is_multiple_of(8) {
        if let Some(last) = v.last_mut() {
            *last = (1u8 << (n % 8)) - 1;
        }
    }
    v
}

/// Write one segment from documents in order (document `i` of the segment is `docs[i]`).
fn write_segment(dir: &Path, docs: &[DocIn]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut count = vec![0u32; 1 << 24];
    for d in docs {
        for &t in &d.tris {
            count[t as usize] += 1;
        }
    }
    let mut keys = Vec::new();
    let mut start = vec![0u32; 1 << 24];
    let mut total = 0u32;
    for t in 0..(1usize << 24) {
        if count[t] > 0 {
            keys.push(t as u32);
            start[t] = total;
            total += count[t];
        }
    }
    let mut post = vec![0u32; total as usize];
    let mut cur = start.clone();
    for (i, d) in docs.iter().enumerate() {
        for &t in &d.tris {
            post[cur[t as usize] as usize] = i as u32;
            cur[t as usize] += 1;
        }
    }
    drop(cur);
    let mut ptr = Vec::with_capacity(keys.len() + 1);
    let mut enc = Vec::with_capacity(post.len() + post.len() / 4);
    for &t in &keys {
        ptr.push(enc.len() as u64);
        let (s, n) = (start[t as usize] as usize, count[t as usize] as usize);
        let mut prev = 0u32;
        for (j, &d) in post[s..s + n].iter().enumerate() {
            varint(&mut enc, if j == 0 { d as u64 } else { (d - prev) as u64 });
            prev = d;
        }
    }
    ptr.push(enc.len() as u64);
    std::fs::write(dir.join("post.bin"), &enc)?;
    npy::save(&dir.join("keys.npy"), &keys, &[keys.len()])?;
    npy::save(&dir.join("ptr.npy"), &ptr, &[ptr.len()])?;
    write_docs(dir, docs.iter().map(|d| (d.rel.as_str(), d.size, d.mtime, d.flags)))
}

fn write_docs<'a>(dir: &Path, docs: impl Iterator<Item = (&'a str, u64, i64, i64)>) -> Result<()> {
    let mut meta: Vec<i64> = Vec::new();
    let mut paths: Vec<u8> = Vec::new();
    let mut poff: Vec<u64> = vec![0];
    for (rel, size, mtime, flags) in docs {
        meta.extend([size as i64, mtime, flags]);
        paths.extend_from_slice(rel.as_bytes());
        poff.push(paths.len() as u64);
    }
    let n = poff.len() - 1;
    std::fs::write(dir.join("paths.bin"), &paths)?;
    npy::save(&dir.join("poff.npy"), &poff, &[poff.len()])?;
    npy::save(&dir.join("docs.npy"), &meta, &[n, 3])?;
    Ok(())
}

/// Merge the live documents of `segs` into one segment at `dir`, streaming key by key.
fn merge_segments(dir: &Path, segs: &[&Segment]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut remap: Vec<Vec<u32>> = Vec::new();
    let mut next = 0u32;
    for s in segs {
        remap.push(
            (0..s.ndocs)
                .map(|d| {
                    if s.is_live(d) {
                        next += 1;
                        next - 1
                    } else {
                        u32::MAX
                    }
                })
                .collect(),
        );
    }
    write_docs(
        dir,
        segs.iter().flat_map(|s| {
            let docs = s.docs();
            (0..s.ndocs).filter(|&d| s.is_live(d)).map(move |d| (s.path(d), docs[d * 3] as u64, docs[d * 3 + 1], docs[d * 3 + 2]))
        }),
    )?;
    let mut w = BufWriter::with_capacity(1 << 20, File::create(dir.join("post.bin"))?);
    let mut keys = Vec::new();
    let mut ptr = vec![0u64];
    let mut cur = vec![0usize; segs.len()];
    let mut buf = Vec::new();
    let mut ids = Vec::new();
    let mut written = 0u64;
    loop {
        let mut min = None;
        for (i, s) in segs.iter().enumerate() {
            if let Some(&k) = s.keys().get(cur[i]) {
                min = Some(min.map_or(k, |m: u32| m.min(k)));
            }
        }
        let Some(k) = min else { break };
        ids.clear();
        for (i, s) in segs.iter().enumerate() {
            if s.keys().get(cur[i]) == Some(&k) {
                let p = s.ptr();
                let post = &bytes(&s.post)[p[cur[i]] as usize..p[cur[i] + 1] as usize];
                decode_into(post, s.ndocs, |d| {
                    if remap[i][d] != u32::MAX {
                        ids.push(remap[i][d]);
                    }
                })?;
                cur[i] += 1;
            }
        }
        if ids.is_empty() {
            continue;
        }
        buf.clear();
        let mut prev = 0u32;
        for (j, &d) in ids.iter().enumerate() {
            varint(&mut buf, if j == 0 { d as u64 } else { (d - prev) as u64 });
            prev = d;
        }
        w.write_all(&buf)?;
        written += buf.len() as u64;
        keys.push(k);
        ptr.push(written);
    }
    w.flush()?;
    npy::save(&dir.join("keys.npy"), &keys, &[keys.len()])?;
    npy::save(&dir.join("ptr.npy"), &ptr, &[ptr.len()])?;
    Ok(())
}

// ------------------------------------------------------------------------- the index
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct TextStats {
    pub files: usize,
    pub files_changed: usize,
    pub files_removed: usize,
    pub segments: usize,
    pub bytes: u64,
    pub seconds: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct Manifest {
    format: u32,
    generation: u64,
    segments: Vec<SegRef>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct SegRef {
    name: String,
    live: String,
}

pub fn dir_of(root: &Path) -> PathBuf {
    root.join(".scrutate").join("text")
}

fn read_manifest(dir: &Path) -> Result<Option<Manifest>> {
    let p = dir.join("manifest.json");
    match std::fs::read(&p) {
        Ok(b) => {
            let m: Manifest = serde_json::from_slice(&b).with_context(|| format!("{} is not a text index manifest", p.display()))?;
            if m.format != FORMAT {
                return Ok(None); // another format: rebuilt by the next update
            }
            Ok(Some(m))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
    }
}

pub struct TextIndex {
    pub root: PathBuf,
    segs: Vec<Segment>,
}

impl TextIndex {
    /// The current snapshot, or None when no text index has been built. A concurrent update may
    /// delete a segment between reading the manifest and opening it: retry with the new manifest.
    pub fn open(root: &Path) -> Result<Option<TextIndex>> {
        let dir = dir_of(root);
        let mut last = None;
        for _ in 0..5 {
            let Some(m) = read_manifest(&dir)? else { return Ok(None) };
            match m.segments.iter().map(|s| Segment::open(&dir, &s.name, &s.live)).collect::<Result<Vec<_>>>() {
                Ok(segs) => return Ok(Some(TextIndex { root: root.to_path_buf(), segs })),
                Err(e) => last = Some(e),
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(last.unwrap_or_else(|| anyhow!("text index unreadable")).context("text index is damaged; rebuild it with `scrutate index --text --force`"))
    }

    pub fn files(&self) -> usize {
        self.segs.iter().map(|s| (0..s.ndocs).filter(|&d| s.is_live(d)).count()).sum()
    }

    pub fn segments(&self) -> usize {
        self.segs.len()
    }

    /// Paths (relative to the root) of the live files that can match `q`. Binary files are included
    /// for multi-line searches (which do not stop at a NUL byte); unreadable and UTF-16 files always.
    pub fn candidates(&self, q: &Q, multiline: bool) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for s in &self.segs {
            let bits = s.eval(q).with_context(|| format!("text index segment {} is damaged; rebuild it with `scrutate index --text --force`", s.name))?;
            for d in 0..s.ndocs {
                if !s.is_live(d) {
                    continue;
                }
                let f = s.flags(d);
                let hit = bits.as_ref().map(|b| b[d / 64] & (1 << (d % 64)) != 0).unwrap_or(true);
                if hit || f & ALWAYS != 0 || (multiline && f & BINARY != 0) {
                    out.push(s.path(d).to_string());
                }
            }
        }
        Ok(out)
    }

    /// Deep validation of every segment (`scrutate check`).
    pub fn check(&self) -> Vec<String> {
        self.segs.iter().filter_map(|s| s.validate(true).err().map(|e| format!("text index segment {}: {e:#}", s.name))).collect()
    }

    pub fn disk_bytes(root: &Path) -> u64 {
        fn walk(p: &Path) -> u64 {
            std::fs::read_dir(p)
                .map(|rd| rd.flatten().map(|e| if e.path().is_dir() { walk(&e.path()) } else { e.metadata().map(|m| m.len()).unwrap_or(0) }).sum())
                .unwrap_or(0)
        }
        walk(&dir_of(root))
    }
}

fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// The files Engine A's default search visits, relative to `root`, with size and mtime.
fn walk(root: &Path) -> Result<Vec<(String, u64, i64)>> {
    let roots = [root.to_path_buf()];
    let walker = instant::build_walker(&roots, &Options::default())?;
    let out = Mutex::new(Vec::new());
    walker.run(|| {
        let (out, roots) = (&out, &roots);
        Box::new(move |entry| {
            if let Ok(e) = entry {
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    let rel = instant::relative(e.path(), roots);
                    let (size, mtime) = e.metadata().map(|m| (m.len(), mtime_ns(&m))).unwrap_or((0, 0));
                    if let Ok(mut v) = out.lock() {
                        v.push((rel, size, mtime));
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });
    let mut v = out.into_inner().map_err(|_| anyhow!("walker panicked"))?;
    v.sort();
    Ok(v)
}

fn next_segment_number(dir: &Path) -> Result<u64> {
    Ok(std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_prefix('s')).and_then(|n| n.parse::<u64>().ok()))
        .max()
        .unwrap_or(0)
        + 1)
}

/// Wait for the update lock. Polls `try_lock`: SMB mounts answer a blocking lock request with
/// EAGAIN instead of waiting, so a blocking `lock()` fails there when another update runs.
fn lock_exclusive(f: &File) -> Result<()> {
    let t0 = Instant::now();
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(anyhow::Error::new(e).context("lock the text index")),
        }
        if t0.elapsed() > std::time::Duration::from_secs(600) {
            bail!("another process has held the text index lock for 10 minutes");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Bring the text index of `root` up to date: unchanged files (same size and mtime) are kept,
/// changed and new files are read into new segments, removed files are marked dead, and the
/// segments are merged when there are too many or too many dead documents.
pub fn update(root: &Path, force: bool, progress: bool) -> Result<TextStats> {
    let t0 = Instant::now();
    let dir = dir_of(root);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let gi = root.join(".scrutate").join(".gitignore");
    if !gi.exists() {
        std::fs::write(&gi, "*\n")?; // the index is a cache; never commit it
    }
    let lock = File::options().create(true).truncate(false).write(true).open(dir.join("lock"))?;
    lock_exclusive(&lock)?;
    let old = if force { None } else { read_manifest(&dir)? };
    let mut segs: Vec<Segment> = match &old {
        Some(m) => m.segments.iter().map(|s| Segment::open(&dir, &s.name, &s.live)).collect::<Result<_>>().unwrap_or_else(|e| {
            eprintln!("[scrutate] text index damaged ({e:#}); rebuilding");
            Vec::new()
        }),
        None => Vec::new(),
    };
    let generation = old.as_ref().map(|m| m.generation).unwrap_or(0) + 1;
    let live_name = format!("live-{generation:08}.bin");
    let files = walk(root)?;
    let mut known: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new();
    for (i, s) in segs.iter().enumerate() {
        for d in 0..s.ndocs {
            if s.is_live(d) {
                known.insert(s.path(d).to_string(), (i, d));
            }
        }
    }
    let mut changed: Vec<(String, u64, i64)> = Vec::new();
    for (rel, size, mtime) in &files {
        match known.remove(rel) {
            Some((i, d)) => {
                let m = segs[i].docs();
                if m[d * 3] as u64 == *size && m[d * 3 + 1] == *mtime && m[d * 3 + 2] & RETRY == 0 {
                    continue;
                }
                segs[i].live[d / 8] &= !(1 << (d % 8));
                changed.push((rel.clone(), *size, *mtime));
            }
            None => changed.push((rel.clone(), *size, *mtime)),
        }
    }
    // what is left in `known` is no longer on disk
    let removed = known.len();
    for (_, (i, d)) in known {
        segs[i].live[d / 8] &= !(1 << (d % 8));
    }
    let mut next_seg = next_segment_number(&dir)?;
    let mut batch: Vec<DocIn> = Vec::new();
    let mut batch_postings = 0usize;
    let total = changed.len();
    let mut done = 0usize;
    let mut flush = |batch: &mut Vec<DocIn>, segs: &mut Vec<Segment>| -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let name = format!("s{next_seg:08}");
        next_seg += 1;
        write_segment(&dir.join(&name), batch)?;
        std::fs::write(dir.join(&name).join(&live_name), all_live(batch.len()))?;
        segs.push(Segment::open(&dir, &name, &live_name)?);
        batch.clear();
        Ok(())
    };
    for chunk in changed.chunks(1024) {
        let docs: Vec<DocIn> = chunk.par_iter().map(|(rel, size, mtime)| read_doc(root, rel.clone(), *size, *mtime)).collect();
        for d in docs {
            batch_postings += d.tris.len();
            batch.push(d);
            if batch_postings >= BATCH_POSTINGS {
                flush(&mut batch, &mut segs)?;
                batch_postings = 0;
            }
        }
        done += chunk.len();
        if progress {
            eprint!("\r[scrutate] text index: {done}/{total} files");
        }
    }
    flush(&mut batch, &mut segs)?;
    if progress && total > 0 {
        eprintln!();
    }
    let live_docs: usize = segs.iter().map(|s| (0..s.ndocs).filter(|&d| s.is_live(d)).count()).sum();
    let all_docs: usize = segs.iter().map(|s| s.ndocs).sum();
    if segs.len() > MAX_SEGMENTS || (all_docs > 0 && (all_docs - live_docs) * 10 > all_docs * 3) {
        let name = format!("s{:08}", next_segment_number(&dir)?);
        merge_segments(&dir.join(&name), &segs.iter().collect::<Vec<_>>())?;
        std::fs::write(dir.join(&name).join(&live_name), all_live(live_docs))?;
        segs = vec![Segment::open(&dir, &name, &live_name)?];
    }
    // liveness of the segments kept: a new file per generation, so a reader of the old manifest
    // keeps a consistent view until it reopens
    for s in &segs {
        let p = dir.join(&s.name).join(&live_name);
        if !p.exists() {
            let tmp = p.with_extension("tmp");
            std::fs::write(&tmp, &s.live)?;
            std::fs::rename(&tmp, &p)?;
        }
    }
    let m = Manifest { format: FORMAT, generation, segments: segs.iter().map(|s| SegRef { name: s.name.clone(), live: live_name.clone() }).collect() };
    let tmp = dir.join("manifest.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&m)?)?;
    std::fs::rename(&tmp, dir.join("manifest.json"))?;
    let names: Vec<String> = segs.iter().map(|s| s.name.clone()).collect();
    let nsegs = segs.len();
    drop(segs);
    gc(&dir, &names, &live_name);
    Ok(TextStats {
        files: live_docs,
        files_changed: total,
        files_removed: removed,
        segments: nsegs,
        bytes: TextIndex::disk_bytes(root),
        seconds: t0.elapsed().as_secs_f64(),
    })
}

/// Remove segment directories and liveness files the manifest no longer names. Errors are ignored
/// (a file still mapped on Windows is removed by a later update).
fn gc(dir: &Path, keep: &[String], live: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !e.path().is_dir() {
            continue;
        }
        if !keep.contains(&name) {
            let _ = std::fs::remove_dir_all(e.path());
            continue;
        }
        if let Ok(files) = std::fs::read_dir(e.path()) {
            for f in files.flatten() {
                let n = f.file_name().to_string_lossy().into_owned();
                if n.starts_with("live-") && n != live {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pat: &str, mode: Mode, case: Case) -> Q {
        query_for(&Options { pattern: pat.to_string(), mode, case, ..Options::default() }).unwrap()
    }

    fn holds(q: &Q, tris: &[u32]) -> bool {
        match q {
            Q::All => true,
            Q::Tri(t) => tris.binary_search(t).is_ok(),
            Q::And(v) => v.iter().all(|q| holds(q, tris)),
            Q::Or(v) => v.iter().any(|q| holds(q, tris)),
        }
    }

    #[test]
    fn literal_needs_all_its_trigrams() {
        let x = q("kernel", Mode::Literal, Case::Sensitive);
        assert!(holds(&x, &trigrams(b"the KERNEL thread")));
        assert!(!holds(&x, &trigrams(b"the kern el")));
        assert_eq!(q("ab", Mode::Literal, Case::Sensitive), Q::All);
    }

    #[test]
    fn alternation_and_classes() {
        let x = q("foo(bar|baz)qux", Mode::Regex, Case::Sensitive);
        assert!(holds(&x, &trigrams(b"xx foobazqux")));
        assert!(!holds(&x, &trigrams(b"foo qux bar")));
        assert_eq!(q("a.*b", Mode::Regex, Case::Sensitive), Q::All);
        let y = q(r"err(or)?_code", Mode::Regex, Case::Sensitive);
        assert!(holds(&y, &trigrams(b"err_code")) && holds(&y, &trigrams(b"error_code")));
    }

    #[test]
    fn unicode_case_folding_is_covered() {
        // (?i)k also matches the Kelvin sign; the condition must accept a file that only has that
        let x = q("kernel", Mode::Literal, Case::Insensitive);
        assert!(holds(&x, &trigrams("\u{212A}ernel".as_bytes())));
        assert!(holds(&x, &trigrams(b"KeRnEl")));
        let y = q("été", Mode::Literal, Case::Insensitive);
        assert!(holds(&y, &trigrams("ÉTÉ".as_bytes())));
    }

    #[test]
    fn identifier_and_fuzzy() {
        let x = q("refreshToken", Mode::Identifier, Case::Sensitive);
        for s in ["refresh_token", "REFRESH-TOKEN", "RefreshToken"] {
            assert!(holds(&x, &trigrams(s.as_bytes())), "{s}");
        }
        let f = q("tokenizer", Mode::Fuzzy { max_edits: 1 }, Case::Sensitive);
        assert!(holds(&f, &trigrams(b"tokXnizer")));
    }

    #[test]
    fn property_condition_never_rejects_a_matching_text() {
        // xorshift: deterministic and dependency-free
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        let atoms = ["ab", "cd", "x", "(ab|cde)", "[a-c]", "k", "s", "é", ".", "\\w", "y?", "(cd)+", "z{2}", "A", "(?:kx|sy)", "[^a]"];
        let alphabet = ["a", "b", "c", "d", "e", "x", "y", "z", "k", "K", "\u{212A}", "s", "ſ", "é", "É", "A", " "];
        for _ in 0..4000 {
            let n = 1 + rnd(5);
            let pat: String = (0..n).map(|_| atoms[rnd(atoms.len())]).collect();
            let case = if rnd(2) == 0 { Case::Insensitive } else { Case::Sensitive };
            let cond = q(&pat, Mode::Regex, case);
            let re = regex::RegexBuilder::new(&pat).case_insensitive(case == Case::Insensitive).build().unwrap();
            for _ in 0..40 {
                let m = rnd(14);
                let text: String = (0..m).map(|_| alphabet[rnd(alphabet.len())]).collect();
                if re.is_match(&text) {
                    assert!(holds(&cond, &trigrams(text.as_bytes())), "pattern {pat:?} ({case:?}) matches {text:?} but the condition {cond:?} rejects it");
                }
            }
        }
    }
}
