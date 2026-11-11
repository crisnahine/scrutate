/*!
The gitignore module provides a way to match globs from a gitignore file
against file paths.

Note that this module implements the specification as described in the
`gitignore` man page from scratch. That is, this module does *not* shell out to
the `git` command line tool.
*/

use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    sync::{Arc, OnceLock},
};

use {
    globset::{Candidate, GlobBuilder, GlobSet, GlobSetBuilder},
    regex_automata::util::pool::Pool,
};

use crate::{
    Error, Match, PartialErrorBuilder,
    pathutil::{is_file_name, strip_prefix},
};

/// Glob represents a single glob in a gitignore file.
///
/// This is used to report information about the highest precedent glob that
/// matched in one or more gitignore files.
#[derive(Clone, Debug)]
pub struct Glob {
    /// The file path that this glob was extracted from.
    from: Option<PathBuf>,
    /// The original glob string.
    original: String,
    /// The actual glob string used to convert to a regex.
    actual: String,
    /// Whether this is a whitelisted glob or not.
    is_whitelist: bool,
    /// Whether this glob should only match directories or not.
    is_only_dir: bool,
    /// The builder's `case_insensitive` and `allow_unclosed_class` when
    /// this glob was added.
    options: (bool, bool),
}

impl Glob {
    /// Returns the file path that defined this glob.
    pub fn from(&self) -> Option<&Path> {
        self.from.as_ref().map(|p| &**p)
    }

    /// The original glob as it was defined in a gitignore file.
    pub fn original(&self) -> &str {
        &self.original
    }

    /// The actual glob that was compiled to respect gitignore
    /// semantics.
    pub fn actual(&self) -> &str {
        &self.actual
    }

    /// Whether this was a whitelisted glob or not.
    pub fn is_whitelist(&self) -> bool {
        self.is_whitelist
    }

    /// Whether this glob must match a directory or not.
    pub fn is_only_dir(&self) -> bool {
        self.is_only_dir
    }

    /// Returns true if and only if this glob has a `**/` prefix.
    fn has_doublestar_prefix(&self) -> bool {
        self.actual.starts_with("**/") || self.actual == "**"
    }
}

/// Gitignore is a matcher for the globs in one or more gitignore files
/// in the same directory.
#[derive(Clone, Debug)]
pub struct Gitignore {
    set: GlobSet,
    /// the index in `globs` of each glob in `set`
    set_index: Vec<usize>,
    /// the globs matched without `set`, with their index in `globs`
    simple: Vec<(usize, Simple)>,
    /// every glob in one set, for a matcher used often (`full_set`)
    full: Arc<Full>,
    root: PathBuf,
    globs: Vec<Glob>,
    num_ignores: u64,
    num_whitelists: u64,
    matches: Option<Arc<Pool<Vec<usize>>>>,
}

/// Every glob of a matcher in one `GlobSet`, compiled once the matcher has
/// tried its simple globs often enough.
#[derive(Debug, Default)]
struct Full {
    /// tries so far, counted in steps of `TICK`
    uses: AtomicUsize,
    /// `None` inside: the set could not be built, and the globs stay as they are
    set: OnceLock<Option<GlobSet>>,
}

/// Tries times simple globs at which the set is compiled: a build costs
/// about 250 µs and trying one simple glob about 15 ns, so this is where
/// going on would cost more than compiling. The Linux tree's root file (over
/// 100 globs, 95,000 paths) compiles after 160 paths; a file with 5 globs
/// in a tree of 3,000 never does.
const COMPILE_AT: usize = 16_000;
/// A thread adds its tries to the shared count once in this many.
const TICK: u32 = 16;

/// A glob that needs no regex: building a `GlobSet` regex costs more than a small
/// tree's whole walk, and most gitignore lines have one of these shapes.
#[derive(Clone, Debug)]
enum Simple {
    /// `**/` and a file name with `*` or `?`: matches the last path component
    Name(Box<[u8]>),
    /// `**/` and a literal path with a slash, stored with a leading `/`
    Tail(Box<[u8]>),
    /// a file name with `*` or `?` anchored at the root (`/x*`): matches a
    /// path of one component
    Root(Box<[u8]>),
}

impl Simple {
    /// The simple form of `actual` (a glob as `add_line` rewrote it), if it
    /// has one. It matches exactly the paths the glob's regex does
    /// (`literal_separator`, `backslash_escape`, case-sensitive).
    fn new(actual: &str) -> Option<Simple> {
        let (anywhere, rest) = match actual.strip_prefix("**/") {
            Some(rest) => (true, rest),
            None => (false, actual),
        };
        if rest.is_empty()
            || rest.contains("**")
            || rest.starts_with('/')
            || rest.ends_with('/')
            || rest.contains("//")
            || rest.bytes().any(|b| matches!(b, b'[' | b']' | b'{' | b'}' | b'\\'))
        {
            return None;
        }
        let wild = rest.bytes().any(|b| b == b'*' || b == b'?');
        match (anywhere, rest.contains('/'), wild) {
            (true, false, true) => Some(Simple::Name(rest.as_bytes().into())),
            (true, true, false) => {
                Some(Simple::Tail(format!("/{rest}").into_bytes().into()))
            }
            (false, false, true) => Some(Simple::Root(rest.as_bytes().into())),
            _ => None,
        }
    }

    /// `name` is the last component of `path`.
    fn is_match(&self, path: &[u8], name: &[u8]) -> bool {
        match self {
            Simple::Name(p) => wildcard(p, name),
            Simple::Tail(t) => path.ends_with(t) || path == &t[1..],
            Simple::Root(p) => name.len() == path.len() && wildcard(p, path),
        }
    }
}

/// `*` matches any bytes, `?` one byte (`name` holds no `/`).
fn wildcard(p: &[u8], name: &[u8]) -> bool {
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < name.len() {
        match p.get(pi) {
            Some(b'*') => {
                star = Some((pi, ni));
                pi += 1;
            }
            Some(&c) if c == b'?' || c == name[ni] => {
                pi += 1;
                ni += 1;
            }
            _ => match star {
                Some((sp, sn)) => {
                    pi = sp + 1;
                    ni = sn + 1;
                    star = Some((sp, sn + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// The bytes globset matches for `path`, with `/` as the separator.
fn path_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::borrow::Cow::Borrowed(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        let mut v = path.to_string_lossy().into_owned().into_bytes();
        for b in v.iter_mut() {
            if *b != b'/' && std::path::is_separator(char::from(*b)) {
                *b = b'/';
            }
        }
        std::borrow::Cow::Owned(v)
    }
}

impl Gitignore {
    /// Creates a new gitignore matcher from the gitignore file path given.
    ///
    /// If it's desirable to include multiple gitignore files in a single
    /// matcher, or read gitignore globs from a different source, then
    /// use `GitignoreBuilder`.
    ///
    /// This always returns a valid matcher, even if it's empty. In particular,
    /// a Gitignore file can be partially valid, e.g., when one glob is invalid
    /// but the rest aren't.
    ///
    /// Note that I/O errors are ignored. For more granular control over
    /// errors, use `GitignoreBuilder`.
    pub fn new<P: AsRef<Path>>(
        gitignore_path: P,
    ) -> (Gitignore, Option<Error>) {
        let path = gitignore_path.as_ref();
        let parent = path.parent().unwrap_or(Path::new("/"));
        let mut builder = GitignoreBuilder::new(parent);
        let mut errs = PartialErrorBuilder::default();
        errs.maybe_push_ignore_io(builder.add(path));
        match builder.build() {
            Ok(gi) => (gi, errs.into_error_option()),
            Err(err) => {
                errs.push(err);
                (Gitignore::empty(), errs.into_error_option())
            }
        }
    }

    /// Creates a new gitignore matcher from the global ignore file, if one
    /// exists.
    ///
    /// The global config file path is specified by git's `core.excludesFile`
    /// config option.
    ///
    /// # Behavior
    ///
    /// This routine does its best to discover any global git exclude files.
    /// This will try to parse out the `excludesFile` config option in your
    /// global git configuration, if necessary.
    ///
    /// The specific things this routine tries (which are subject to change
    /// based on how git behaves) are:
    ///
    ///
    ///
    /// Git's config file location is `$HOME/.gitconfig`. If `$HOME/.gitconfig`
    /// does not exist or does not specify `core.excludesFile`, then
    /// `$XDG_CONFIG_HOME/git/ignore` is read. If `$XDG_CONFIG_HOME` is not
    /// set or is empty, then `$HOME/.config/git/ignore` is used instead.
    pub fn global() -> (Gitignore, Option<Error>) {
        match std::env::current_dir() {
            Ok(cwd) => GitignoreBuilder::new(cwd).build_global(),
            Err(err) => (Gitignore::empty(), Some(err.into())),
        }
    }

    /// Creates a new empty gitignore matcher that never matches anything.
    ///
    /// Its path is empty.
    pub fn empty() -> Gitignore {
        Gitignore {
            set: GlobSet::empty(),
            set_index: vec![],
            simple: vec![],
            full: Arc::default(),
            root: PathBuf::from(""),
            globs: vec![],
            num_ignores: 0,
            num_whitelists: 0,
            matches: None,
        }
    }

    /// Returns the directory containing this gitignore matcher.
    ///
    /// All matches are done relative to this path.
    pub fn path(&self) -> &Path {
        &*self.root
    }

    /// Returns true if and only if this gitignore has zero globs, and
    /// therefore never matches any file path.
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.simple.is_empty()
    }

    /// Returns the total number of globs, which should be equivalent to
    /// `num_ignores + num_whitelists`.
    pub fn len(&self) -> usize {
        self.globs.len()
    }

    /// Returns the total number of ignore globs.
    pub fn num_ignores(&self) -> u64 {
        self.num_ignores
    }

    /// Returns the total number of whitelisted globs.
    pub fn num_whitelists(&self) -> u64 {
        self.num_whitelists
    }

    /// Returns whether the given path (file or directory) matched a pattern in
    /// this gitignore matcher.
    ///
    /// `is_dir` should be true if the path refers to a directory and false
    /// otherwise.
    ///
    /// The given path is matched relative to the path given when building
    /// the matcher. Specifically, before matching `path`, its prefix (as
    /// determined by a common suffix of the directory containing this
    /// gitignore) is stripped. If there is no common suffix/prefix overlap,
    /// then `path` is assumed to be relative to this matcher.
    pub fn matched<P: AsRef<Path>>(
        &self,
        path: P,
        is_dir: bool,
    ) -> Match<&Glob> {
        if self.is_empty() {
            return Match::None;
        }
        self.matched_stripped(self.strip(path.as_ref()), is_dir)
    }

    /// Returns whether the given path (file or directory, and expected to be
    /// under the root) or any of its parent directories (up to the root)
    /// matched a pattern in this gitignore matcher.
    ///
    /// NOTE: This method is more expensive than walking the directory hierarchy
    /// top-to-bottom and matching the entries. But, is easier to use in cases
    /// when a list of paths are available without a hierarchy.
    ///
    /// `is_dir` should be true if the path refers to a directory and false
    /// otherwise.
    ///
    /// The given path is matched relative to the path given when building
    /// the matcher. Specifically, before matching `path`, its prefix (as
    /// determined by a common suffix of the directory containing this
    /// gitignore) is stripped. If there is no common suffix/prefix overlap,
    /// then `path` is assumed to be relative to this matcher.
    ///
    /// # Panics
    ///
    /// This method panics if the given file path is not under the root path
    /// of this matcher.
    pub fn matched_path_or_any_parents<P: AsRef<Path>>(
        &self,
        path: P,
        is_dir: bool,
    ) -> Match<&Glob> {
        if self.is_empty() {
            return Match::None;
        }
        let mut path = self.strip(path.as_ref());
        assert!(!path.has_root(), "path is expected to be under the root");

        match self.matched_stripped(path, is_dir) {
            Match::None => (), // walk up
            a_match => return a_match,
        }
        while let Some(parent) = path.parent() {
            match self.matched_stripped(parent, /* is_dir */ true) {
                Match::None => path = parent, // walk up
                a_match => return a_match,
            }
        }
        Match::None
    }

    /// Like matched, but takes a path that has already been stripped.
    fn matched_stripped<P: AsRef<Path>>(
        &self,
        path: P,
        is_dir: bool,
    ) -> Match<&Glob> {
        if self.is_empty() {
            return Match::None;
        }
        let path = path.as_ref();
        // the last glob that matches (and applies to this kind of entry) wins
        let applies = |i: usize| !self.globs[i].is_only_dir() || is_dir;
        let mut best = None;
        if let Some(full) = self.full_set() {
            let mut matches = self.matches.as_ref().unwrap().get();
            let candidate = Candidate::new(path);
            full.matches_candidate_into(&candidate, &mut *matches);
            best = matches.iter().rev().copied().find(|&i| applies(i));
        } else {
            if !self.set.is_empty() {
                let mut matches = self.matches.as_ref().unwrap().get();
                let candidate = Candidate::new(path);
                self.set.matches_candidate_into(&candidate, &mut *matches);
                best = matches
                    .iter()
                    .rev()
                    .map(|&i| self.set_index[i])
                    .find(|&i| applies(i));
            }
            if !self.simple.is_empty() {
                let bytes = path_bytes(path);
                let name = match memchr::memrchr(b'/', &bytes) {
                    Some(i) => &bytes[i + 1..],
                    None => &bytes[..],
                };
                for (i, s) in self.simple.iter().rev() {
                    if best.is_some_and(|b| b > *i) {
                        break;
                    }
                    if applies(*i) && s.is_match(&bytes, name) {
                        best = Some(*i);
                        break;
                    }
                }
            }
        }
        match best.map(|i| &self.globs[i]) {
            Some(glob) if glob.is_whitelist() => Match::Whitelist(glob),
            Some(glob) => Match::Ignore(glob),
            None => Match::None,
        }
    }

    /// The set of every glob, once trying the simple globs one by one has
    /// cost about as much as compiling it (`COMPILE_AT`); `None` before.
    fn full_set(&self) -> Option<&GlobSet> {
        if self.simple.is_empty() {
            return None;
        }
        if let Some(set) = self.full.set.get() {
            return set.as_ref();
        }
        thread_local!(static TRIES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) });
        let tries = TRIES.with(|t| {
            t.set(t.get().wrapping_add(1));
            t.get()
        });
        if tries % TICK != 0 {
            return None;
        }
        let uses = self.full.uses.fetch_add(TICK as usize, Ordering::Relaxed)
            + TICK as usize;
        if uses * self.simple.len() < COMPILE_AT {
            return None;
        }
        self.full.set.get_or_init(|| self.compile()).as_ref()
    }

    fn compile(&self) -> Option<GlobSet> {
        let mut builder = GlobSetBuilder::new();
        for glob in &self.globs {
            let (case_insensitive, unclosed_class) = glob.options;
            builder.add(
                GlobBuilder::new(&glob.actual)
                    .literal_separator(true)
                    .case_insensitive(case_insensitive)
                    .backslash_escape(true)
                    .allow_unclosed_class(unclosed_class)
                    .build()
                    .ok()?,
            );
        }
        builder.build().ok()
    }

    /// Strips the given path such that it's suitable for matching with this
    /// gitignore matcher.
    fn strip<'a, P: 'a + AsRef<Path> + ?Sized>(
        &'a self,
        path: &'a P,
    ) -> &'a Path {
        let mut path = path.as_ref();
        // A leading ./ is completely superfluous. We also strip it from
        // our gitignore root path, so we need to strip it from our candidate
        // path too.
        if let Some(p) = strip_prefix("./", path) {
            path = p;
        }
        // Strip any common prefix between the candidate path and the root
        // of the gitignore, to make sure we get relative matching right.
        // BUT, a file name might not have any directory components to it,
        // in which case, we don't want to accidentally strip any part of the
        // file name.
        //
        // As an additional special case, if the root is just `.`, then we
        // shouldn't try to strip anything, e.g., when path begins with a `.`.
        if self.root != Path::new(".") && !is_file_name(path) {
            if let Some(p) = strip_prefix(&self.root, path) {
                path = p;
                // If we're left with a leading slash, get rid of it.
                if let Some(p) = strip_prefix("/", path) {
                    path = p;
                }
            }
        }
        path
    }
}

/// Builds a matcher for a single set of globs from a .gitignore file.
#[derive(Clone, Debug)]
pub struct GitignoreBuilder {
    builder: GlobSetBuilder,
    set_index: Vec<usize>,
    simple: Vec<(usize, Simple)>,
    root: PathBuf,
    globs: Vec<Glob>,
    case_insensitive: bool,
    allow_unclosed_class: bool,
}

impl GitignoreBuilder {
    /// Create a new builder for a gitignore file.
    ///
    /// The path given should be the path at which the globs for this gitignore
    /// file should be matched. Note that paths are always matched relative
    /// to the root path given here. Generally, the root path should correspond
    /// to the *directory* containing a `.gitignore` file.
    pub fn new<P: AsRef<Path>>(root: P) -> GitignoreBuilder {
        let root = root.as_ref();
        GitignoreBuilder {
            builder: GlobSetBuilder::new(),
            set_index: vec![],
            simple: vec![],
            root: strip_prefix("./", root).unwrap_or(root).to_path_buf(),
            globs: vec![],
            case_insensitive: false,
            allow_unclosed_class: true,
        }
    }

    /// Builds a new matcher from the globs added so far.
    ///
    /// Once a matcher is built, no new globs can be added to it.
    pub fn build(&self) -> Result<Gitignore, Error> {
        if self.globs.is_empty() {
            return Ok(Gitignore::empty());
        }
        let nignore = self.globs.iter().filter(|g| !g.is_whitelist()).count();
        let nwhite = self.globs.iter().filter(|g| g.is_whitelist()).count();
        let set = self
            .builder
            .build()
            .map_err(|err| Error::Glob { glob: None, err: err.to_string() })?;
        Ok(Gitignore {
            set,
            set_index: self.set_index.clone(),
            simple: self.simple.clone(),
            full: Arc::default(),
            root: self.root.clone(),
            globs: self.globs.clone(),
            num_ignores: nignore as u64,
            num_whitelists: nwhite as u64,
            matches: Some(Arc::new(
                Pool::with_available_parallelism_capacity(|| vec![]),
            )),
        })
    }

    /// Build a global gitignore matcher using the configuration in this
    /// builder.
    ///
    /// This consumes ownership of the builder unlike `build` because it
    /// must mutate the builder to add the global gitignore globs.
    ///
    /// Note that this ignores the path given to this builder's constructor
    /// and instead derives the path automatically from git's global
    /// configuration.
    pub fn build_global(mut self) -> (Gitignore, Option<Error>) {
        match gitconfig_excludes_path() {
            None => (Gitignore::empty(), None),
            Some(path) => {
                if !path.is_file() {
                    (Gitignore::empty(), None)
                } else {
                    let mut errs = PartialErrorBuilder::default();
                    errs.maybe_push_ignore_io(self.add(path));
                    match self.build() {
                        Ok(gi) => (gi, errs.into_error_option()),
                        Err(err) => {
                            errs.push(err);
                            (Gitignore::empty(), errs.into_error_option())
                        }
                    }
                }
            }
        }
    }

    /// Add each glob from the file path given.
    ///
    /// The file given should be formatted as a `gitignore` file.
    ///
    /// Note that partial errors can be returned. For example, if there was
    /// a problem adding one glob, an error for that will be returned, but
    /// all other valid globs will still be added.
    pub fn add<P: AsRef<Path>>(&mut self, path: P) -> Option<Error> {
        let path = path.as_ref();
        let file = match File::open(path) {
            Err(err) => return Some(Error::Io(err).with_path(path)),
            Ok(file) => file,
        };
        log::debug!("opened gitignore file: {}", path.display());
        let rdr = BufReader::new(file);
        let mut errs = PartialErrorBuilder::default();
        for (i, line) in rdr.lines().enumerate() {
            let lineno = (i + 1) as u64;
            let line = match line {
                Ok(line) => line,
                Err(err) => {
                    errs.push(Error::Io(err).tagged(path, lineno));
                    break;
                }
            };

            // Match Git's handling of .gitignore files that begin with the Unicode BOM
            const UTF8_BOM: &str = "\u{feff}";
            let line =
                if i == 0 { line.trim_start_matches(UTF8_BOM) } else { &line };

            if let Err(err) = self.add_line(Some(path.to_path_buf()), &line) {
                errs.push(err.tagged(path, lineno));
            }
        }
        errs.into_error_option()
    }

    /// Add each glob line from the string given.
    ///
    /// If this string came from a particular `gitignore` file, then its path
    /// should be provided here.
    ///
    /// The string given should be formatted as a `gitignore` file.
    #[cfg(test)]
    fn add_str(
        &mut self,
        from: Option<PathBuf>,
        gitignore: &str,
    ) -> Result<&mut GitignoreBuilder, Error> {
        for line in gitignore.lines() {
            self.add_line(from.clone(), line)?;
        }
        Ok(self)
    }

    /// Add a line from a gitignore file to this builder.
    ///
    /// If this line came from a particular `gitignore` file, then its path
    /// should be provided here.
    ///
    /// If the line could not be parsed as a glob, then an error is returned.
    pub fn add_line(
        &mut self,
        from: Option<PathBuf>,
        mut line: &str,
    ) -> Result<&mut GitignoreBuilder, Error> {
        #![allow(deprecated)]

        if line.starts_with("#") {
            return Ok(self);
        }
        if !line.ends_with("\\ ") {
            line = line.trim_right();
        }
        if line.is_empty() {
            return Ok(self);
        }
        let mut glob = Glob {
            from,
            original: line.to_string(),
            actual: String::new(),
            is_whitelist: false,
            is_only_dir: false,
            options: (self.case_insensitive, self.allow_unclosed_class),
        };
        let mut is_absolute = false;
        if line.starts_with("\\!") || line.starts_with("\\#") {
            line = &line[1..];
            is_absolute = line.chars().nth(0) == Some('/');
        } else {
            if line.starts_with("!") {
                glob.is_whitelist = true;
                line = &line[1..];
            }
            if line.starts_with("/") {
                // `man gitignore` says that if a glob starts with a slash,
                // then the glob can only match the beginning of a path
                // (relative to the location of gitignore). We achieve this by
                // simply banning wildcards from matching /.
                line = &line[1..];
                is_absolute = true;
            }
        }
        // If it ends with a slash, then this should only match directories,
        // but the slash should otherwise not be used while globbing.
        if line.as_bytes().last() == Some(&b'/') {
            glob.is_only_dir = true;
            line = &line[..line.len() - 1];
            // If the slash was escaped, then remove the escape.
            // See: https://github.com/BurntSushi/ripgrep/issues/2236
            if line.as_bytes().last() == Some(&b'\\') {
                line = &line[..line.len() - 1];
            }
        }
        glob.actual = line.to_string();
        // If there is a literal slash, then this is a glob that must match the
        // entire path name. Otherwise, we should let it match anywhere, so use
        // a **/ prefix.
        if !is_absolute && !line.chars().any(|c| c == '/') {
            // ... but only if we don't already have a **/ prefix.
            if !glob.has_doublestar_prefix() {
                glob.actual = format!("**/{}", glob.actual);
            }
        }
        // If the glob ends with `/**`, then we should only match everything
        // inside a directory, but not the directory itself. Standard globs
        // will match the directory. So we add `/*` to force the issue.
        if glob.actual.ends_with("/**") {
            glob.actual = format!("{}/*", glob.actual);
        }
        if !self.case_insensitive
            && let Some(s) = Simple::new(&glob.actual)
        {
            self.simple.push((self.globs.len(), s));
            self.globs.push(glob);
            return Ok(self);
        }
        let parsed = GlobBuilder::new(&glob.actual)
            .literal_separator(true)
            .case_insensitive(self.case_insensitive)
            .backslash_escape(true)
            .allow_unclosed_class(self.allow_unclosed_class)
            .build()
            .map_err(|err| Error::Glob {
                glob: Some(glob.original.clone()),
                err: err.kind().to_string(),
            })?;
        self.builder.add(parsed);
        self.set_index.push(self.globs.len());
        self.globs.push(glob);
        Ok(self)
    }

    /// Toggle whether the globs should be matched case insensitively or not.
    ///
    /// When this option is changed, only globs added after the change will be
    /// affected.
    ///
    /// This is disabled by default.
    pub fn case_insensitive(
        &mut self,
        yes: bool,
    ) -> Result<&mut GitignoreBuilder, Error> {
        // TODO: This should not return a `Result`. Fix this in the next semver
        // release.
        self.case_insensitive = yes;
        Ok(self)
    }

    /// Toggle whether unclosed character classes are allowed. When allowed,
    /// a `[` without a matching `]` is treated literally instead of resulting
    /// in a parse error.
    ///
    /// For example, if this is set then the glob `[abc` will be treated as the
    /// literal string `[abc` instead of returning an error.
    ///
    /// By default, this is true in order to match established `gitignore`
    /// semantics. Generally speaking, enabling this leads to worse failure
    /// modes since the glob parser becomes more permissive. You might want to
    /// enable this when compatibility (e.g., with POSIX glob implementations)
    /// is more important than good error messages.
    pub fn allow_unclosed_class(
        &mut self,
        yes: bool,
    ) -> &mut GitignoreBuilder {
        self.allow_unclosed_class = yes;
        self
    }
}

/// Return the file path of the current environment's global gitignore file.
///
/// Note that the file path returned may not exist.
pub fn gitconfig_excludes_path() -> Option<PathBuf> {
    // When GIT_CONFIG_GLOBAL is set, it replaces both $HOME/.gitconfig and
    // $XDG_CONFIG_HOME/git/config (per git 2.32+). Otherwise, git supports
    // $HOME/.gitconfig and $XDG_CONFIG_HOME/git/config simultaneously, where
    // $HOME/.gitconfig takes precedent.
    gitconfig_global_env_contents()
        .and_then(|x| parse_excludes_file(&x))
        .or_else(|| {
            gitconfig_home_contents().and_then(|x| parse_excludes_file(&x))
        })
        .or_else(|| {
            gitconfig_xdg_contents().and_then(|x| parse_excludes_file(&x))
        })
        // System-level config has the lowest priority for core.excludesFile.
        // GIT_CONFIG_SYSTEM overrides the default /etc/gitconfig path.
        .or_else(|| {
            gitconfig_system_contents().and_then(|x| parse_excludes_file(&x))
        })
        .or_else(excludes_file_default)
}

/// Returns the file contents of git's global config file from the path
/// specified by the `GIT_CONFIG_GLOBAL` environment variable.
fn gitconfig_global_env_contents() -> Option<Vec<u8>> {
    let path = std::env::var_os("GIT_CONFIG_GLOBAL").map(PathBuf::from)?;
    if path.as_os_str().is_empty() {
        return None;
    }
    let mut file = BufReader::new(File::open(path).ok()?);
    let mut contents = vec![];
    file.read_to_end(&mut contents).ok().map(|_| contents)
}

/// Returns the file contents of git's system-level config file.
///
/// Checks `GIT_CONFIG_SYSTEM` first, then falls back to `/etc/gitconfig`.
fn gitconfig_system_contents() -> Option<Vec<u8>> {
    let path = std::env::var_os("GIT_CONFIG_SYSTEM")
        .map(PathBuf::from)
        .filter(|x| !x.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/etc/gitconfig"));
    let mut file = BufReader::new(File::open(path).ok()?);
    let mut contents = vec![];
    file.read_to_end(&mut contents).ok().map(|_| contents)
}

/// Returns the file contents of git's global config file, if one exists, in
/// the user's home directory.
fn gitconfig_home_contents() -> Option<Vec<u8>> {
    let home = home_dir()?;
    let mut file = BufReader::new(File::open(home.join(".gitconfig")).ok()?);
    let mut contents = vec![];
    file.read_to_end(&mut contents).ok().map(|_| contents)
}

/// Returns the file contents of git's global config file, if one exists, in
/// the user's XDG_CONFIG_HOME directory.
fn gitconfig_xdg_contents() -> Option<Vec<u8>> {
    let path = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|x| !x.as_os_str().is_empty())
        .or_else(|| home_dir().map(|p| p.join(".config")))
        .map(|x| x.join("git/config"))?;
    let mut file = BufReader::new(File::open(path).ok()?);
    let mut contents = vec![];
    file.read_to_end(&mut contents).ok().map(|_| contents)
}

/// Returns the default file path for a global .gitignore file.
///
/// Specifically, this respects XDG_CONFIG_HOME.
fn excludes_file_default() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|x| !x.as_os_str().is_empty())
        .or_else(|| home_dir().map(|p| p.join(".config")))
        .map(|x| x.join("git/ignore"))
}

/// Extract git's `core.excludesfile` config setting from the raw file contents
/// given.
fn parse_excludes_file(data: &[u8]) -> Option<PathBuf> {
    let span = excludes_file_span(data)?;
    let candidate = &data[span];
    std::str::from_utf8(candidate).ok().map(|s| PathBuf::from(expand_tilde(s)))
}

/// The capture of `(?im-u)^\s*excludesfile\s*=\s*"?\s*(\S+?)\s*"?\s*$` in
/// `data` (leftmost match), found by hand: compiling that regex took most of
/// a small search's run time.
fn excludes_file_span(data: &[u8]) -> Option<std::ops::Range<usize>> {
    let ws = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r');
    let skip_ws = |mut i: usize| {
        while i < data.len() && ws(data[i]) {
            i += 1;
        }
        i
    };
    // `\s*"?\s*$` from `i`: whitespace with at most one quote, then a line end
    let tail_ok = |mut i: usize| {
        let mut quote = false;
        loop {
            if i == data.len() || data[i] == b'\n' {
                return true;
            }
            if ws(data[i]) {
                i += 1;
            } else if data[i] == b'"' && !quote {
                quote = true;
                i += 1;
            } else {
                return false;
            }
        }
    };
    // the shortest non-empty run of non-whitespace from `t` that `tail_ok` accepts
    let token = |t: usize| {
        let mut e = t;
        while e < data.len() && !ws(data[e]) {
            e += 1;
            if tail_ok(e) {
                return Some(t..e);
            }
        }
        None
    };
    let key = b"excludesfile";
    let mut start = 0;
    loop {
        let mut i = skip_ws(start);
        if data.len() - i >= key.len() && data[i..i + key.len()].eq_ignore_ascii_case(key) {
            i = skip_ws(i + key.len());
            if i < data.len() && data[i] == b'=' {
                i = skip_ws(i + 1);
                let quoted = (i < data.len() && data[i] == b'"').then(|| token(skip_ws(i + 1))).flatten();
                if let Some(span) = quoted.or_else(|| token(i)) {
                    return Some(span);
                }
            }
        }
        start += memchr::memchr(b'\n', &data[start..])? + 1;
    }
}

/// Expands ~ in file paths to the value of $HOME.
fn expand_tilde(path: &str) -> String {
    let home = match home_dir() {
        None => return path.to_string(),
        Some(home) => home.to_string_lossy().into_owned(),
    };
    path.replace("~", &home)
}

/// Returns the location of the user's home directory.
fn home_dir() -> Option<PathBuf> {
    // We're fine with using std::env::home_dir for now. Its bugs are, IMO,
    // pretty minor corner cases.
    #![allow(deprecated)]
    std::env::home_dir()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Gitignore, GitignoreBuilder};

    fn gi_from_str<P: AsRef<Path>>(root: P, s: &str) -> Gitignore {
        let mut builder = GitignoreBuilder::new(root);
        builder.add_str(None, s).unwrap();
        builder.build().unwrap()
    }

    macro_rules! ignored {
        ($name:ident, $root:expr, $gi:expr, $path:expr) => {
            ignored!($name, $root, $gi, $path, false);
        };
        ($name:ident, $root:expr, $gi:expr, $path:expr, $is_dir:expr) => {
            #[test]
            fn $name() {
                let gi = gi_from_str($root, $gi);
                assert!(gi.matched($path, $is_dir).is_ignore());
            }
        };
    }

    macro_rules! not_ignored {
        ($name:ident, $root:expr, $gi:expr, $path:expr) => {
            not_ignored!($name, $root, $gi, $path, false);
        };
        ($name:ident, $root:expr, $gi:expr, $path:expr, $is_dir:expr) => {
            #[test]
            fn $name() {
                let gi = gi_from_str($root, $gi);
                assert!(!gi.matched($path, $is_dir).is_ignore());
            }
        };
    }

    const ROOT: &'static str = "/home/foobar/rust/rg";

    ignored!(ig1, ROOT, "months", "months");
    ignored!(ig2, ROOT, "*.lock", "Cargo.lock");
    ignored!(ig3, ROOT, "*.rs", "src/main.rs");
    ignored!(ig4, ROOT, "src/*.rs", "src/main.rs");
    ignored!(ig5, ROOT, "/*.c", "cat-file.c");
    ignored!(ig6, ROOT, "/src/*.rs", "src/main.rs");
    ignored!(ig7, ROOT, "!src/main.rs\n*.rs", "src/main.rs");
    ignored!(ig8, ROOT, "foo/", "foo", true);
    ignored!(ig9, ROOT, "**/foo", "foo");
    ignored!(ig10, ROOT, "**/foo", "src/foo");
    ignored!(ig11, ROOT, "**/foo/**", "src/foo/bar");
    ignored!(ig12, ROOT, "**/foo/**", "wat/src/foo/bar/baz");
    ignored!(ig13, ROOT, "**/foo/bar", "foo/bar");
    ignored!(ig14, ROOT, "**/foo/bar", "src/foo/bar");
    ignored!(ig15, ROOT, "abc/**", "abc/x");
    ignored!(ig16, ROOT, "abc/**", "abc/x/y");
    ignored!(ig17, ROOT, "abc/**", "abc/x/y/z");
    ignored!(ig18, ROOT, "a/**/b", "a/b");
    ignored!(ig19, ROOT, "a/**/b", "a/x/b");
    ignored!(ig20, ROOT, "a/**/b", "a/x/y/b");
    ignored!(ig21, ROOT, r"\!xy", "!xy");
    ignored!(ig22, ROOT, r"\#foo", "#foo");
    ignored!(ig23, ROOT, "foo", "./foo");
    ignored!(ig24, ROOT, "target", "grep/target");
    ignored!(ig25, ROOT, "Cargo.lock", "./tabwriter-bin/Cargo.lock");
    ignored!(ig26, ROOT, "/foo/bar/baz", "./foo/bar/baz");
    ignored!(ig27, ROOT, "foo/", "xyz/foo", true);
    ignored!(ig28, "./src", "/llvm/", "./src/llvm", true);
    ignored!(ig29, ROOT, "node_modules/ ", "node_modules", true);
    ignored!(ig30, ROOT, "**/", "foo/bar", true);
    ignored!(ig31, ROOT, "path1/*", "path1/foo");
    ignored!(ig32, ROOT, ".a/b", ".a/b");
    ignored!(ig33, "./", ".a/b", ".a/b");
    ignored!(ig34, ".", ".a/b", ".a/b");
    ignored!(ig35, "./.", ".a/b", ".a/b");
    ignored!(ig36, "././", ".a/b", ".a/b");
    ignored!(ig37, "././.", ".a/b", ".a/b");
    ignored!(ig38, ROOT, "\\[", "[");
    ignored!(ig39, ROOT, "\\?", "?");
    ignored!(ig40, ROOT, "\\*", "*");
    ignored!(ig41, ROOT, "\\a", "a");
    ignored!(ig42, ROOT, "s*.rs", "sfoo.rs");
    ignored!(ig43, ROOT, "**", "foo.rs");
    ignored!(ig44, ROOT, "**/**/*", "a/foo.rs");

    not_ignored!(ignot1, ROOT, "amonths", "months");
    not_ignored!(ignot2, ROOT, "monthsa", "months");
    not_ignored!(ignot3, ROOT, "/src/*.rs", "src/grep/src/main.rs");
    not_ignored!(ignot4, ROOT, "/*.c", "mozilla-sha1/sha1.c");
    not_ignored!(ignot5, ROOT, "/src/*.rs", "src/grep/src/main.rs");
    not_ignored!(ignot6, ROOT, "*.rs\n!src/main.rs", "src/main.rs");
    not_ignored!(ignot7, ROOT, "foo/", "foo", false);
    not_ignored!(ignot8, ROOT, "**/foo/**", "wat/src/afoo/bar/baz");
    not_ignored!(ignot9, ROOT, "**/foo/**", "wat/src/fooa/bar/baz");
    not_ignored!(ignot10, ROOT, "**/foo/bar", "foo/src/bar");
    not_ignored!(ignot11, ROOT, "#foo", "#foo");
    not_ignored!(ignot12, ROOT, "\n\n\n", "foo");
    not_ignored!(ignot13, ROOT, "foo/**", "foo", true);
    not_ignored!(
        ignot14,
        "./third_party/protobuf",
        "m4/ltoptions.m4",
        "./third_party/protobuf/csharp/src/packages/repositories.config"
    );
    not_ignored!(ignot15, ROOT, "!/bar", "foo/bar");
    not_ignored!(ignot16, ROOT, "*\n!**/", "foo", true);
    not_ignored!(ignot17, ROOT, "src/*.rs", "src/grep/src/main.rs");
    not_ignored!(ignot18, ROOT, "path1/*", "path2/path1/foo");
    not_ignored!(ignot19, ROOT, "s*.rs", "src/foo.rs");

    fn bytes(s: &str) -> Vec<u8> {
        s.to_string().into_bytes()
    }

    fn path_string<P: AsRef<Path>>(path: P) -> String {
        path.as_ref().to_str().unwrap().to_string()
    }

    #[test]
    fn parse_excludes_file1() {
        let data = bytes("[core]\nexcludesFile = /foo/bar");
        let got = super::parse_excludes_file(&data).unwrap();
        assert_eq!(path_string(got), "/foo/bar");
    }

    #[test]
    fn parse_excludes_file2() {
        let data = bytes("[core]\nexcludesFile = ~/foo/bar");
        let got = super::parse_excludes_file(&data).unwrap();
        assert_eq!(path_string(got), super::expand_tilde("~/foo/bar"));
    }

    #[test]
    fn parse_excludes_file3() {
        let data = bytes("[core]\nexcludeFile = /foo/bar");
        assert!(super::parse_excludes_file(&data).is_none());
    }

    #[test]
    fn parse_excludes_file4() {
        let data = bytes("[core]\nexcludesFile = \"~/foo/bar\"");
        let got = super::parse_excludes_file(&data);
        assert_eq!(
            path_string(got.unwrap()),
            super::expand_tilde("~/foo/bar")
        );
    }

    #[test]
    fn parse_excludes_file5() {
        let data = bytes("[core]\nexcludesFile = \" \"~/foo/bar \" \"");
        assert!(super::parse_excludes_file(&data).is_none());
    }

    #[test]
    fn excludes_file_span_equals_the_regex() {
        use regex_automata::{meta::Regex, util::syntax};
        let re = Regex::builder()
            .configure(Regex::config().utf8_empty(false))
            .syntax(syntax::Config::new().utf8(false))
            .build(r#"(?im-u)^\s*excludesfile\s*=\s*"?\s*(\S+?)\s*"?\s*$"#)
            .unwrap();
        let parts: &[&[u8]] = &[
            b"excludesfile", b"ExcludesFile", b"excludesfil", b" ", b"\t", b"\n", b"\r", b"\x0b",
            b"=", b"\"", b"/foo", b"~/bar", b"x", b"[core]", b"path", b"\xff", b"# c",
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        let mut matched = 0;
        for _ in 0..300_000 {
            let mut data = Vec::new();
            if next(2) == 0 {
                data.extend_from_slice(parts[next(parts.len())]);
                data.extend_from_slice(b"excludesFile");
                data.extend_from_slice(parts[3 + next(4)]);
                data.push(b'=');
            }
            for _ in 0..next(10) {
                data.extend_from_slice(parts[next(parts.len())]);
            }
            let mut caps = re.create_captures();
            re.captures(&data, &mut caps);
            let want = caps.get_group(1).map(|s| s.range());
            matched += want.is_some() as usize;
            assert_eq!(super::excludes_file_span(&data), want, "{:?}", bstr::BStr::new(&data));
        }
        assert!(matched > 10_000, "{matched}");
    }

    /// Random gitignore lines and paths: the matcher (some globs matched
    /// without a regex) gives the same answer as one GlobSet of every glob.
    #[test]
    fn simple_globs_equal_the_glob_set() {
        use globset::{Candidate, GlobBuilder, GlobSetBuilder};
        let line_parts =
            ["a", "b", "*", "?", ".", "x*y", "é", "a*", "*.b", "/", "[ab]", "**", "\\*", "{a,b}"];
        let path_parts = ["a", "b", "ab", ".a", "x", "xay", "é", "a.b", "xy", "*", ""];
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        let (mut simple, mut hits, mut compiled) = (0, 0, 0);
        for _ in 0..3_000 {
            let mut text = String::new();
            for _ in 0..1 + next(4) {
                let mut line = String::new();
                if next(4) == 0 {
                    line.push('!');
                }
                for _ in 0..1 + next(4) {
                    line.push_str(line_parts[next(line_parts.len())]);
                }
                if next(5) == 0 {
                    line.push('/');
                }
                text.push_str(&line);
                text.push('\n');
            }
            let mut b = GitignoreBuilder::new(ROOT);
            if b.add_str(None, &text).is_err() {
                continue;
            }
            let gi = b.build().unwrap();
            simple += gi.simple.len();
            let mut sb = GlobSetBuilder::new();
            for g in &gi.globs {
                sb.add(
                    GlobBuilder::new(&g.actual)
                        .literal_separator(true)
                        .backslash_escape(true)
                        .allow_unclosed_class(true)
                        .build()
                        .unwrap(),
                );
            }
            let set = sb.build().unwrap();
            let paths: Vec<(String, bool)> = (0..30)
                .map(|_| {
                    let n = 1 + next(3);
                    let path = (0..n)
                        .map(|_| path_parts[next(path_parts.len())])
                        .collect::<Vec<_>>()
                        .join("/");
                    (path, next(2) == 0)
                })
                .collect();
            // the same paths again once the matcher has switched to its one set
            let passes = if gi.simple.is_empty() { 1 } else { 2 };
            for (pass, (path, is_dir)) in
                (0..passes).flat_map(|p| paths.iter().map(move |x| (p, x)))
            {
                if pass == 1 && gi.full.set.get().is_none() {
                    let _ = gi.full.set.set(gi.compile());
                    assert!(gi.full_set().is_some(), "{text}");
                    compiled += 1;
                }
                let (path, is_dir) = (path.as_str(), *is_dir);
                let want = set
                    .matches_candidate(&Candidate::new(&path))
                    .into_iter()
                    .rev()
                    .map(|i| &gi.globs[i])
                    .find(|g| !g.is_only_dir() || is_dir)
                    .map(|g| (g.is_whitelist(), g.original().to_string()));
                let got = match gi.matched(path, is_dir) {
                    super::Match::None => None,
                    super::Match::Ignore(g) => Some((false, g.original().to_string())),
                    super::Match::Whitelist(g) => Some((true, g.original().to_string())),
                };
                hits += want.is_some() as usize;
                assert_eq!(got, want, "path {path:?} dir {is_dir} in\n{text}");
            }
        }
        assert!(
            simple > 1_000 && hits > 5_000 && compiled > 500,
            "{simple} simple globs, {hits} hits, {compiled} compiled"
        );
    }

    #[test]
    fn a_busy_matcher_compiles_its_globs_and_answers_the_same() {
        use crate::Match;
        let gi = gi_from_str(
            ROOT,
            "*.o\n!keep*.o\n/top*\nsub/dir\nbuild/\n[ab].txt\n",
        );
        let cases = [
            ("x.o", false),
            ("keep1.o", false),
            ("a/keep.o", false),
            ("top1", false),
            ("a/top1", false),
            ("x/sub/dir", false),
            ("sub/dir", true),
            ("build", true),
            ("build", false),
            ("a.txt", false),
            ("c.txt", false),
            ("z", false),
        ];
        let answers = |gi: &Gitignore| -> Vec<u8> {
            cases
                .iter()
                .map(|(path, is_dir)| match gi.matched(path, *is_dir) {
                    Match::None => 0,
                    Match::Ignore(_) => 1,
                    Match::Whitelist(_) => 2,
                })
                .collect()
        };
        assert!(gi.full.set.get().is_none());
        let before = answers(&gi);
        assert_eq!(before, [1, 2, 2, 1, 0, 0, 1, 1, 0, 1, 0, 0]);
        for _ in 0..super::COMPILE_AT {
            gi.matched("z", false);
        }
        assert!(gi.full.set.get().is_some_and(|set| set.is_some()));
        assert_eq!(answers(&gi), before);
    }

    // See: https://github.com/BurntSushi/ripgrep/issues/106
    #[test]
    fn regression_106() {
        gi_from_str("/", " ");
    }

    #[test]
    fn case_insensitive() {
        let gi = GitignoreBuilder::new(ROOT)
            .case_insensitive(true)
            .unwrap()
            .add_str(None, "*.html")
            .unwrap()
            .build()
            .unwrap();
        assert!(gi.matched("foo.html", false).is_ignore());
        assert!(gi.matched("foo.HTML", false).is_ignore());
        assert!(!gi.matched("foo.htm", false).is_ignore());
        assert!(!gi.matched("foo.HTM", false).is_ignore());
    }

    ignored!(cs1, ROOT, "*.html", "foo.html");
    not_ignored!(cs2, ROOT, "*.html", "foo.HTML");
    not_ignored!(cs3, ROOT, "*.html", "foo.htm");
    not_ignored!(cs4, ROOT, "*.html", "foo.HTM");
}
