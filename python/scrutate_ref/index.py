"""Incremental on-disk index: SQLite (chunks, call refs) + memory-mapped numpy segments (BM25, vectors)."""
from __future__ import annotations

import hashlib
import json
import multiprocessing as mp
import os
import shutil
import sqlite3
import subprocess
import sys
import time
import zlib
from collections import Counter
from dataclasses import dataclass

import numpy as np

from .lex import LexStore, SegmentBuilder, merge_segments, pack_bags, quantize
from .parse import is_indexable, parse_source
from .semantic import SemanticModel, default_model, default_model_path
from .text import BODY_CAP, is_test_path, name_key, tokenize2


def pack_text(text: str) -> bytes:
    """Chunk text as stored: zlib-compressed UTF-8 (2.4x smaller on source code; ~6 us to inflate)."""
    return zlib.compress(text.encode("utf8"), 6)


def unpack_text(blob) -> str:
    if blob is None:
        return ""
    if isinstance(blob, str):
        return blob
    return zlib.decompress(blob).decode("utf8")

SCHEMA_VERSION = 13
# What the parser and tokenizer make of a file; the engine's index.rs holds the same value. Bump it
# with every change of the rules in parse.py or text.py (and langs.rs, parse.rs, text.rs): an index
# of an older revision reads every file again on its next update; a newer one is not updated.
CONTENT_REVISION = 2
MAX_SEGMENTS = 6          # more live segments than this triggers a merge of the small ones
SMALL_INDEX_ROWS = 8000   # an index this small is kept as ONE segment (a rewrite costs a few ms)
GC_AGE_S = 600            # unreferenced segment directories younger than this may be in flight
MAX_FILE_BYTES = 1_000_000
TF_CAP = 4  # a token counts at most this many times per field (keeps the index small)
SKIP_DIRS = {".git", ".scrutate", "node_modules", "__pycache__", ".venv", "venv", "dist", "build",
             ".mypy_cache", ".pytest_cache", "target", ".next", ".tox", ".idea", ".vscode"}
SKIP_UNLESS_GIT = {"build", "dist", "target", "venv"}  # some projects commit these

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime_ns INTEGER, hash TEXT);
CREATE TABLE IF NOT EXISTS chunks(
  id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT, name TEXT, qualname TEXT,
  name_key TEXT, qual_key TEXT, start INTEGER, end INTEGER, is_test INTEGER,
  ndoc INTEGER, nbody INTEGER, text BLOB, name_tok TEXT);
CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path, start);
CREATE INDEX IF NOT EXISTS chunks_name ON chunks(name_key);
CREATE INDEX IF NOT EXISTS chunks_qual ON chunks(qual_key);
CREATE TABLE IF NOT EXISTS refs(chunk_id INTEGER NOT NULL, callee TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS refs_callee ON refs(callee);
CREATE INDEX IF NOT EXISTS refs_chunk ON refs(chunk_id);
CREATE TABLE IF NOT EXISTS imports(path TEXT NOT NULL, module TEXT NOT NULL, line INTEGER);
CREATE INDEX IF NOT EXISTS imports_path ON imports(path);
CREATE INDEX IF NOT EXISTS imports_module ON imports(module);
"""


@dataclass
class UpdateStats:
    files_total: int = 0
    files_changed: int = 0
    files_removed: int = 0
    chunks: int = 0
    seconds: float = 0.0
    semantic: bool = False


def split_names(v: str) -> list[str]:
    """A comma-separated list of names; `\\,` is a comma inside a name."""
    out, cur, i = [], "", 0
    while i < len(v):
        if v[i] == "\\" and v[i + 1 : i + 2] == ",":
            cur, i = cur + ",", i + 2
            continue
        if v[i] == ",":
            out.append(cur)
            cur = ""
        else:
            cur += v[i]
        i += 1
    out.append(cur)
    return [s.strip() for s in out if s.strip()]


def list_files(root: str) -> list[str]:
    """Relative paths of candidate files, honoring .gitignore when ripgrep exists."""
    files: list[str] = []
    if shutil.which("rg"):
        try:
            # hidden files are code too (.github/workflows, .eslintrc.js); SKIP_DIRS prunes caches and VCS;
            # --no-config: a ripgrep configuration file must not change the indexed set
            out = subprocess.run(["rg", "--files", "--hidden", "--no-config", "--no-messages", "--glob", "!.scrutate", "--glob", "!.git"],
                                 cwd=root, capture_output=True, text=True, check=False).stdout
            files = [l for l in out.splitlines() if l]
        except OSError:
            files = []
    skip = (SKIP_DIRS - SKIP_UNLESS_GIT) if _in_git(root) else set(SKIP_DIRS)
    skip |= set(split_names(os.environ.get("SCRUTATE_INDEX_EXCLUDE_DIRS", "")))
    if not files:
        for dp, dns, fns in os.walk(root):
            dns[:] = [d for d in dns if d not in skip]
            files.extend(os.path.relpath(os.path.join(dp, fn), root) for fn in fns
                         if fn != ".git" and not os.path.islink(os.path.join(dp, fn)))
    return sorted(f for f in files if is_indexable(f)
                  and not any(p in skip for p in f.split(os.sep)))


def _in_git(root: str) -> bool:
    d = os.path.abspath(root)
    while True:
        if os.path.exists(os.path.join(d, ".git")):
            return True
        if os.path.dirname(d) == d:
            return False
        d = os.path.dirname(d)


# ------------------------------------------------------------------------ workers
_W_MODEL: SemanticModel | None = None


def _worker_init(model_path: str | None) -> None:
    global _W_MODEL
    _W_MODEL = SemanticModel.load(model_path) if model_path else None


def _bag(tokens: list[str], cap: int | None = None) -> Counter:
    """{token: frequency}; with `cap` a token counts at most that many times (keeps big files cheap)."""
    c = Counter(tokens)
    if cap is not None:
        for t, n in c.items():
            if n > cap:
                c[t] = cap
    return c


def build_rows(rel: str, source: str, model: SemanticModel | None):
    """Parse + tokenize one file -> (rows, FileLex). Pure function; runs in worker processes.

    rows[i] = (chunk columns, sorted callee names); FileLex holds the same chunks' token bags
    and semantic vectors in the layout the segment writer wants."""
    is_test = int(is_test_path(rel))
    path_s, _ = tokenize2(rel.replace("/", " ").replace(".", " ").replace("_", " "))
    path_bag = Counter(path_s)
    rows, bags, vecs = [], [], []
    for c in parse_source(rel, source):
        qual = c.qualname.rsplit(".", 1)[0] if "." in c.qualname else ""
        name_s, name_w = tokenize2(c.name)
        qual_s, qual_w = tokenize2(qual)
        sig_s, sig_w = tokenize2(c.sig, 80)
        doc_s, doc_w = tokenize2(c.doc, 200)
        body_s, body_w = tokenize2(c.body, BODY_CAP)
        rows.append(((c.kind, c.name, c.qualname, name_key(c.name), name_key(c.qualname), c.start, c.end,
                      is_test, len(doc_s), len(body_s), pack_text(c.text), " ".join(dict.fromkeys(name_s))), sorted(c.calls)))
        if c.kind == "constant":  # a symbol only: no postings, no vector, never a ranked candidate
            continue
        bags.append((Counter(name_s + name_w), Counter(qual_s + qual_w), _bag(sig_s + sig_w, TF_CAP),
                     _bag(doc_s + doc_w, TF_CAP), _bag(body_s + body_w, TF_CAP), path_bag))
        if model is not None:
            z = np.zeros((2, model.k), np.float32)
            if c.kind != "window":
                za = model.encode_chunk(name_s, qual_s, sig_s, doc_s, [])
                if za is not None:
                    z[0] = za
            zb = model.encode_chunk([], [], [], [], body_s)  # window chunks of text files: body view only
            if zb is not None:
                z[1] = zb
            vecs.append(z)
    return rows, pack_bags(bags, quantize(np.stack(vecs)) if vecs else None)


def decode_source(raw):
    """UTF-16 with a byte-order mark is transcoded; a NUL in the first 4 KB means binary (None)."""
    if raw[:2] in (b"\xff\xfe", b"\xfe\xff"):
        return raw[2:].decode("utf-16-le" if raw[:2] == b"\xff\xfe" else "utf-16-be", "replace")
    if b"\x00" in raw[:4096]:
        return None
    return raw.decode("utf8", "replace")


def _process(args):
    """-> (rel, status, payload). status: 'skip' | 'same' | 'new' | 'gone'."""
    rel, root, known_hash = args
    full = os.path.join(root, rel)
    try:
        st = os.stat(full)
        if st.st_size > MAX_FILE_BYTES:
            return rel, "skip", (st.st_size, st.st_mtime_ns)
        with open(full, "rb") as f:
            raw = f.read()
    except OSError:
        return rel, "gone", None
    source = decode_source(raw)
    if source is None:
        return rel, "skip", (st.st_size, st.st_mtime_ns)
    h = hashlib.sha1(raw).hexdigest()
    if h == known_hash:
        return rel, "same", (st.st_size, st.st_mtime_ns, h)
    try:
        rows, lex = build_rows(rel, source, _W_MODEL)
    except Exception as e:  # a parser bug must never abort indexing
        print(f"[scrutate-ref] skipped {rel}: {e}", file=sys.stderr)
        return rel, "skip", (st.st_size, st.st_mtime_ns)
    return rel, "new", (st.st_size, st.st_mtime_ns, h, rows, lex)


# --------------------------------------------------------------------------- index
class Index:
    def __init__(self, root: str, model: SemanticModel | None | str = "default"):
        self.root = os.path.abspath(root)
        self.dir = os.path.join(self.root, ".scrutate")
        self.seg_dir = os.path.join(self.dir, "seg")
        os.makedirs(self.dir, exist_ok=True)
        gi = os.path.join(self.dir, ".gitignore")
        if not os.path.exists(gi):
            with open(gi, "w") as f:
                f.write("*\n")  # the index is a cache; never commit it
        self.model = default_model() if model == "default" else model
        self._model_path = (os.environ.get("SCRUTATE_SEMANTIC", default_model_path())
                            if model == "default" else None)
        self._db_path = os.path.join(self.dir, "index.db")
        sem_sig = self._model_signature()
        if not self._compatible(sem_sig):
            self._wipe()  # other layout, other semantic model or damaged files -> rebuild from scratch
        os.makedirs(self.seg_dir, exist_ok=True)
        self.db = sqlite3.connect(self._db_path, isolation_level=None,
                                  check_same_thread=False, timeout=30)  # callers serialize access (MCP uses threads)
        self.db.row_factory = sqlite3.Row
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=OFF")
        self.db.execute("PRAGMA temp_store=MEMORY")
        self.db.execute("PRAGMA cache_size=-32000")
        self.db.executescript(SCHEMA)
        self.set_meta("schema_version", str(SCHEMA_VERSION))
        self.set_meta("model_sig", sem_sig)
        if self.meta("content_rev") != str(CONTENT_REVISION) and not self.db.execute("SELECT 1 FROM files LIMIT 1").fetchone():
            self.set_meta("content_rev", str(CONTENT_REVISION))
        self._lex: LexStore | None = None
        self._lex_ver: str | None = None
        self._last_refresh = 0.0
        self._scan_cost = 1.0  # seconds the last refresh took (see refresh)
        self._gc_segments()

    def _compatible(self, sem_sig: str) -> bool:
        """True when there is no index yet, or the stored one can be reused as is."""
        if not os.path.exists(self._db_path):
            return True
        try:
            con = sqlite3.connect(self._db_path, timeout=30)
            try:
                got = dict(con.execute("SELECT key, value FROM meta WHERE key IN "
                                       "('schema_version','model_sig','segments')").fetchall())
            finally:
                con.close()
        except sqlite3.Error:
            return False
        if got.get("schema_version") != str(SCHEMA_VERSION) or got.get("model_sig") != sem_sig:
            return False
        try:
            names = json.loads(got.get("segments") or "[]")
        except ValueError:
            return False
        return all(os.path.isdir(os.path.join(self.seg_dir, n)) for n in names)

    def _wipe(self) -> None:
        for name in ("index.db", "index.db-wal", "index.db-shm", "vectors.npz"):
            try:
                os.remove(os.path.join(self.dir, name))
            except FileNotFoundError:
                pass
        shutil.rmtree(self.seg_dir, ignore_errors=True)

    # ---- meta helpers
    def meta(self, key: str) -> str | None:
        r = self.db.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchone()
        return r[0] if r else None

    def set_meta(self, key: str, value: str) -> None:
        self.db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?,?)", (key, value))

    def _model_signature(self) -> str:
        if self.model is None:
            return "none"
        if self.model.file_sig:
            return self.model.file_sig
        return f"{self.model.k}:{self.model.cfg.get('n_pairs')}:{len(self.model.va)}:{len(self.model.vb)}"

    # ---- segments
    def segments(self) -> list[str]:
        return json.loads(self.meta("segments") or "[]")

    def _new_segment_name(self) -> str:
        n = int(self.meta("seg_counter") or 0) + 1
        self.set_meta("seg_counter", str(n))
        return f"s{n:06d}"

    def _gc_segments(self) -> None:
        """Delete segment directories no manifest refers to (left by a crash), unless brand new."""
        keep = set(self.segments())
        now = time.time()
        for name in os.listdir(self.seg_dir):
            path = os.path.join(self.seg_dir, name)
            if name not in keep and now - os.path.getmtime(path) > GC_AGE_S:
                shutil.rmtree(path, ignore_errors=True)

    @property
    def lex(self) -> LexStore:
        """The segment store, with statistics for the chunks that are alive right now."""
        ver = self.meta("data_version") or "0"
        names = self.segments()
        if self._lex is None or self._lex.names != names:
            self._lex = LexStore(self.seg_dir, names)
            self._lex_ver = None
        if self._lex_ver != ver:
            n_rows = sum(s.n for s in self._lex.segs)
            n_live = self.db.execute("SELECT COUNT(*) FROM chunks WHERE kind != 'constant'").fetchone()[0]
            # as many segment rows as chunks: nothing was ever deleted, so skip reading every id
            live = None if n_rows == n_live else np.fromiter(
                (r[0] for r in self.db.execute("SELECT id FROM chunks ORDER BY id")), np.int64)
            self._lex.prepare(live)
            self._lex_ver = ver
        return self._lex

    # ---- update
    def update(self, force: bool = False, workers: int | None = None) -> UpdateStats:
        t0 = time.time()
        st = UpdateStats(semantic=self.model is not None)
        rev = self.meta("content_rev") or ""
        if not force and rev.isdigit() and int(rev) > CONTENT_REVISION:
            return st  # a newer engine's parsing rules: read the index as it is, never update it
        changed = False
        builder = SegmentBuilder()
        seg_name = None
        removed: list[int] = []  # ids of the chunks this update deletes
        new_ver = None
        self.db.execute("BEGIN IMMEDIATE")  # one writer at a time; readers keep working (WAL)
        try:
            # if the statistics held in memory describe exactly the state we start from, they can be
            # patched afterwards instead of recomputed
            in_sync = (self._lex is not None and self._lex_ver == (self.meta("data_version") or "0")
                       and self._lex.names == self.segments())
            reparse = self.meta("content_rev") != str(CONTENT_REVISION)
            force = force or reparse
            scan_t0 = time.monotonic()
            known = {r["path"]: (r["size"], r["mtime_ns"], r["hash"])
                     for r in self.db.execute("SELECT path,size,mtime_ns,hash FROM files")}
            files = list_files(self.root)
            st.files_total = len(files)
            gone_files = sorted(set(known) - set(files))
            todo = []
            for rel in files:
                try:
                    s = os.stat(os.path.join(self.root, rel))
                except OSError:
                    continue
                k = known.get(rel)
                if not force and k and k[0] == s.st_size and k[1] == s.st_mtime_ns:
                    continue  # stat unchanged: no read, no hash
                todo.append((rel, self.root, None if force else (k[2] if k else None)))
            self._scan_cost = time.monotonic() - scan_t0  # what looking for changes costs, whatever it finds
            next_id = int(self.meta("next_id") or 1)  # ids are never reused: a segment row is identified by its id
            nworkers = workers if workers is not None else min(os.cpu_count() or 1, 8)
            if len(todo) < 24:
                nworkers = 1
            for gone in gone_files:
                removed += self._delete_file(gone)
                st.files_removed += 1
                changed = True
            for rel, status, payload in self._run(todo, nworkers):
                if status == "gone":
                    continue
                if status == "skip":  # binary / too large / unparsable: remember so we never re-read it
                    removed += self._delete_file(rel)
                    self.db.execute("INSERT OR REPLACE INTO files(path,size,mtime_ns,hash) VALUES(?,?,?,'SKIP')",
                                    (rel, *payload))
                    continue
                if status == "same":  # content identical, only the mtime moved
                    self.db.execute("UPDATE files SET size=?, mtime_ns=? WHERE path=?", (*payload[:2], rel))
                    continue
                size, mtime, h, rows, lex = payload
                removed += self._delete_file(rel)
                first = next_id
                next_id = self._insert(rel, rows, next_id)
                builder.add_file(first, lex)
                self.db.execute("INSERT OR REPLACE INTO files(path,size,mtime_ns,hash) VALUES(?,?,?,?)",
                                (rel, size, mtime, h))
                st.files_changed += 1
                changed = True
            if builder.n:
                seg_name = self._new_segment_name()
                builder.write(os.path.join(self.seg_dir, seg_name))
                self.set_meta("segments", json.dumps(self.segments() + [seg_name]))
            self.set_meta("next_id", str(next_id))
            if reparse:
                self.set_meta("content_rev", str(CONTENT_REVISION))
            changed = changed or bool(removed)
            if changed:
                new_ver = self._bump_version()  # same transaction as the manifest: readers never see one without the other
            self.db.execute("COMMIT")
        except BaseException:
            self.db.execute("ROLLBACK")
            if seg_name:
                shutil.rmtree(os.path.join(self.seg_dir, seg_name), ignore_errors=True)
            raise
        if changed:
            if in_sync:
                try:
                    self._lex.apply(np.asarray(removed, np.int64), [seg_name] if seg_name else [])
                    self._lex_ver = new_ver
                except Exception:  # whatever went wrong, the next search recomputes from the files
                    self._lex = self._lex_ver = None
            self._compact()
            self.db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        st.chunks = self.db.execute("SELECT COUNT(*) FROM chunks").fetchone()[0]
        st.seconds = time.time() - t0
        return st

    def refresh(self, min_interval: float = 1.0) -> UpdateStats | None:
        """`update()` for long-running processes (MCP server, agent): files edited since the last
        search become searchable. The directory is scanned at most once per `min_interval` seconds,
        and a scan that is cheap (a few ms on a small repository) is not held back that long: the
        wait is capped at ten times what looking for changes cost last time."""
        now = time.monotonic()
        if now - self._last_refresh < min(min_interval, 10 * self._scan_cost):
            return None
        st = self.update()
        self._last_refresh = time.monotonic()
        return st

    def _bump_version(self) -> str:
        v = str(int(self.meta("data_version") or 0) + 1)
        self.set_meta("data_version", v)
        return v

    def _compact(self) -> None:
        """Drop dead segments and merge small ones so queries touch few files (see MAX_SEGMENTS)."""
        store = self.lex
        counts, sizes = store.alive_counts(), [s.n for s in store.segs]
        live = [i for i, c in enumerate(counts) if c > 0]
        merge: list[int] = []
        if live:
            big = max(live, key=lambda i: counts[i])
            small = [i for i in live if i != big]
            rows = sum(sizes[i] for i in live)
            dead_frac = 1.0 - sum(counts[i] for i in live) / rows
            small_alive = sum(counts[i] for i in small)
            if rows <= SMALL_INDEX_ROWS:
                merge = live if (len(live) > 1 or dead_frac > 0) else []
            elif dead_frac > 0.3 or small_alive > 0.25 * counts[big]:
                merge = live
            elif len(live) > MAX_SEGMENTS and len(small) > 1:
                merge = small
        if len(live) == len(store.names) and not merge:
            return
        new_names = [store.names[i] for i in live if i not in merge]
        dest = None
        if merge:
            self.db.execute("BEGIN")
            dest = self._new_segment_name()
            self.db.execute("COMMIT")
            merge_segments(os.path.join(self.seg_dir, dest), [store.segs[i] for i in merge],
                           [store.alive[i] for i in merge])
            new_names.append(dest)
        self.db.execute("BEGIN")
        self.set_meta("segments", json.dumps(new_names))
        self._bump_version()
        self.db.execute("COMMIT")
        self._lex = None
        for name in set(store.names) - set(new_names):
            shutil.rmtree(os.path.join(self.seg_dir, name), ignore_errors=True)

    def _run(self, todo, nworkers):
        global _W_MODEL
        _W_MODEL = self.model  # inherited by forked workers
        if self.model is not None and todo:
            self.model.warm()  # build the weight matrices once, here, rather than in every worker
        method = "fork" if sys.platform.startswith("linux") else "spawn"
        model_path = self._model_path if (self.model is not None and self._model_path) else None
        if method == "spawn" and self.model is not None and model_path is None:
            nworkers = 1  # a custom in-memory model cannot be shipped to spawned workers
        if nworkers <= 1:
            for a in todo:
                yield _process(a)
            return
        init, initargs = (None, ()) if method == "fork" else (_worker_init, (model_path,))
        with mp.get_context(method).Pool(nworkers, initializer=init, initargs=initargs) as pool:
            yield from pool.imap(_process, todo, chunksize=8)  # in file order: chunk ids, and so ties, as in the Rust engine

    def _insert(self, rel: str, rows, next_id: int) -> int:
        crow, rrow = [], []
        for chunk_row, calls in rows:
            cid = next_id
            next_id += 1
            crow.append((cid, rel) + chunk_row)
            rrow.extend((cid, c) for c in calls)
        self.db.executemany("INSERT INTO chunks(id,path,kind,name,qualname,name_key,qual_key,start,end,"
                            "is_test,ndoc,nbody,text,name_tok) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)", crow)
        if rrow:
            self.db.executemany("INSERT INTO refs(chunk_id,callee) VALUES(?,?)", rrow)
        return next_id

    def _delete_file(self, rel: str) -> list[int]:
        """Chunks vanish from SQLite; their segment rows simply stop being alive (see lex.prepare).
        Returns the ids that died."""
        ids = [r[0] for r in self.db.execute("SELECT id FROM chunks WHERE path=?", (rel,))]
        if ids:
            for i in range(0, len(ids), 500):
                part = ids[i : i + 500]
                self.db.execute(f"DELETE FROM refs WHERE chunk_id IN ({','.join('?' * len(part))})", part)
            self.db.execute("DELETE FROM chunks WHERE path=?", (rel,))
        self.db.execute("DELETE FROM imports WHERE path=?", (rel,))
        self.db.execute("DELETE FROM files WHERE path=?", (rel,))
        return ids

    # ---- lookups
    def chunk(self, cid: int):
        return self.db.execute("SELECT * FROM chunks WHERE id=?", (cid,)).fetchone()

    def chunk_at(self, path: str, line: int):
        """Smallest non-window chunk containing the line (falls back to windows)."""
        return self.db.execute(
            "SELECT * FROM chunks WHERE path=? AND start<=? AND end>=? "
            "ORDER BY (kind='window'), (end-start) LIMIT 1", (path, line, line)).fetchone()

    def stats(self) -> dict:
        q = lambda s: self.db.execute(s).fetchone()[0]  # noqa: E731
        return {"files": q("SELECT COUNT(*) FROM files"), "chunks": q("SELECT COUNT(*) FROM chunks"),
                "symbols": q("SELECT COUNT(*) FROM chunks WHERE kind NOT IN ('window','module')"),
                "call_edges": q("SELECT COUNT(*) FROM refs"),
                "semantic": self.model is not None}
