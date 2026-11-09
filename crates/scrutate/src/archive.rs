//! What `grep -z` reads: the members of zip and tar archives, and the contents of compressed
//! files. gzip (alone or around a tar) is decompressed in process; bzip2, xz, zstd, lz4, brotli and
//! `.Z` by the system's own tools, as ripgrep's `-z` does.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Decompressed bytes allowed per archive or compressed file: a small zip bomb must not fill memory
/// or keep a search busy for hours.
pub const MAX_EXPANDED: u64 = 8 << 30;
/// Members searched per archive.
pub const MAX_MEMBERS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Zip,
    Tar,
    TarGz,
    /// a tar inside another compression format
    TarOther,
    Gz,
    /// one compressed file, decompressed by an external tool
    Other,
}

const OTHER: &[&str] = &[".bz2", ".tbz", ".tbz2", ".xz", ".txz", ".lzma", ".zst", ".zstd", ".tzst", ".lz4", ".br", ".z"];

impl Kind {
    pub fn of(path: &str) -> Option<Kind> {
        let l = path.to_ascii_lowercase();
        let tar_inside = |ext: &str| l.strip_suffix(ext).is_some_and(|s| s.ends_with(".tar"));
        Some(if l.ends_with(".zip") {
            Kind::Zip
        } else if l.ends_with(".tar") {
            Kind::Tar
        } else if l.ends_with(".tgz") || tar_inside(".gz") {
            Kind::TarGz
        } else if l.ends_with(".gz") {
            Kind::Gz
        } else if [".tbz", ".tbz2", ".txz", ".tzst"].iter().any(|e| l.ends_with(e)) || OTHER.iter().any(|e| tar_inside(e)) {
            Kind::TarOther
        } else if OTHER.iter().any(|e| l.ends_with(e)) && !(l.ends_with(".z") && !path.ends_with(".Z")) {
            Kind::Other
        } else {
            return None;
        })
    }
}

/// Counts what is read and fails past `MAX_EXPANDED`.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.left = self.left.checked_sub(n as u64).ok_or_else(|| std::io::Error::other(format!("expands beyond {} GiB; stopped", MAX_EXPANDED >> 30)))?;
        Ok(n)
    }
}

fn external(path: &Path) -> std::io::Result<Box<dyn Read>> {
    let r = grep_cli::DecompressionReaderBuilder::new().build(path).map_err(std::io::Error::other)?;
    Ok(Box::new(r))
}

/// Calls `each(member, reader)` for every regular file inside `path` (member None: the
/// decompressed file itself). `each` returns false to stop. Errors of one member are passed to
/// `failed` and the walk goes on; an error that ends the walk is returned.
pub fn walk(path: &Path, kind: Kind, each: &mut dyn FnMut(Option<&str>, &mut dyn Read) -> bool, failed: &mut dyn FnMut(String)) -> std::io::Result<()> {
    match kind {
        Kind::Gz => {
            let mut r = Capped { inner: flate2::read::MultiGzDecoder::new(File::open(path)?), left: MAX_EXPANDED };
            each(None, &mut r);
            Ok(())
        }
        Kind::Other => {
            let mut r = Capped { inner: external(path)?, left: MAX_EXPANDED };
            each(None, &mut r);
            Ok(())
        }
        Kind::Tar => tar(Capped { inner: Box::new(File::open(path)?) as Box<dyn Read>, left: MAX_EXPANDED }, each),
        Kind::TarGz => tar(Capped { inner: Box::new(flate2::read::MultiGzDecoder::new(File::open(path)?)) as Box<dyn Read>, left: MAX_EXPANDED }, each),
        Kind::TarOther => tar(Capped { inner: external(path)?, left: MAX_EXPANDED }, each),
        Kind::Zip => {
            let mut z = zip::ZipArchive::new(File::open(path)?).map_err(std::io::Error::other)?;
            let mut left = MAX_EXPANDED;
            for i in 0..z.len().min(MAX_MEMBERS) {
                let mut e = match z.by_index(i) {
                    Ok(e) => e,
                    Err(err) => {
                        failed(format!("member {i}: {err}"));
                        continue;
                    }
                };
                if !e.is_file() {
                    continue;
                }
                let name = e.name().map(|n| n.into_owned()).unwrap_or_else(|_| format!("member {i}"));
                let mut r = Capped { inner: &mut e, left };
                if !each(Some(&name), &mut r) {
                    break;
                }
                left = r.left;
            }
            Ok(())
        }
    }
}

fn tar<R: Read>(r: R, each: &mut dyn FnMut(Option<&str>, &mut dyn Read) -> bool) -> std::io::Result<()> {
    let mut a = tar::Archive::new(r);
    for (n, entry) in a.entries()?.enumerate() {
        if n >= MAX_MEMBERS {
            break;
        }
        let mut e = entry?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        let name = e.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        if !each(Some(&name), &mut e) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_name() {
        for (name, kind) in [
            ("a.zip", Some(Kind::Zip)),
            ("a.tar", Some(Kind::Tar)),
            ("a.tar.gz", Some(Kind::TarGz)),
            ("A.TGZ", Some(Kind::TarGz)),
            ("a.gz", Some(Kind::Gz)),
            ("a.tar.xz", Some(Kind::TarOther)),
            ("a.tbz2", Some(Kind::TarOther)),
            ("a.log.zst", Some(Kind::Other)),
            ("a.Z", Some(Kind::Other)),
            ("a.z", None),
            ("a.txt", None),
            ("zip", None),
        ] {
            assert_eq!(Kind::of(name), kind, "{name}");
        }
    }

    #[test]
    fn capped_reader_stops() {
        let data = [b'a'; 100];
        let mut r = Capped { inner: &data[..], left: 10 };
        let mut buf = Vec::new();
        assert!(r.read_to_end(&mut buf).is_err());
    }
}
