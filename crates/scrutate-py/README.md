# scrutate (Python bindings)

Python access to the Scrutate engines through PyO3, built with maturin against the stable ABI
(`abi3`, CPython 3.9+): one wheel per operating system and architecture.

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
