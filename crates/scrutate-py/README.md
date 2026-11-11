# scrutate (Python bindings)

Python access to the Scrutate engines through PyO3, built with maturin against the stable ABI:
per operating system and architecture, one `abi3` wheel for standard CPython 3.9 to 3.14, one
`abi3t` wheel for 3.15 and newer (standard and free-threaded), and a `cp314t` wheel for
free-threaded 3.14.

```python
import scrutate

# Engine A: index-free search, streamed as the files are scanned
for m in scrutate.grep("refresh_token", ["."], mode="literal"):
    print(m.path, m.line, m.text)

# Engine B: the persistent code-intelligence index
repo = scrutate.Repo(".")
repo.index()                       # incremental; releases the GIL while it runs
for hit in repo.search("where do we refresh expired tokens", k=5):
    print(hit.path, hit.start, hit.qualname, hit.score)
```

The GIL is released during indexing and searching, so several Python threads can drive the
engines in parallel. `grep` returns an iterator fed by a background search thread; results are
converted to Python objects one batch at a time, never materialised as one giant list.
