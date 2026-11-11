# Ranking quality (Engine B)

How good are the answers, as opposed to how fast? Numbers below are produced by
`bench/train_ranker.py eval v8` (log: `test_eval_v8_py.log`, 2026-11-09) and
`bench/train_ranker.py compare v8` (log: `compare_v8.log`) on packages that were never used to
train the semantic model, fit the ranker or choose hyper-parameters. The Rust engine produces
bit-identical candidates, features and scores to the Python code that ran this evaluation, so the
numbers hold for the shipped binary. `bench/parity.py` checks it with no tolerance: any difference
in the chunk tables, a feature, a score, a top 10 or the end-to-end result fails, and so does a
run that cannot make as many queries as asked. Its queries are built from the corpus's documented
functions by the benchmark's rules: 300 on networkx and 50 on the engine's own sources
(`crates/scrutate/src`, a CI step). `bench/parity_repos.sh` runs it with 40 queries on 19 real
repositories of the six languages the reference has a grammar for; parity indexes only a
repository's files in those six languages.

## Protocol

- A query is generated from a real function; the function it came from is the one correct answer.
- The indexed copy has that function's docstring removed (**variant A**: every docstring in the
  package removed, an undocumented code base; **variant B**: only the first sentence of each
  docstring removed), so the answer text is not sitting in the index.
- Query types: **doc** = the first docstring sentence written as a natural-language request;
  **kw** = three content words sampled from it; **name** = the function name as words
  (`remove tool`); **id** = the bare identifier (`remove_tool`); **qid** = the qualified name
  (`Server.remove_tool`).
- **MRR@10**: mean of 1/rank of the correct result (0 when not in the top 10). **hit@k**: share of
  queries with the correct result in the top k. Brackets: 95 % bootstrap intervals over queries.
- Held-out packages: networkx 3.4.2, mcp 1.30.0, rich 15.0.0 (Python), and ten repositories in
  other languages (next section). Compared systems run on the **same candidate
  pool, tokenizer, stemming and field weights**: *BM25* is Scrutate's own lexical layer alone;
  *semantic only* ranks by the vector similarity alone; *linear blend* is the hand-set fallback
  used when no ranker is present; *learned* is the shipped pipeline.

## Variant A: every docstring removed (MRR@10, n queries)

| Query type | n | BM25 | semantic only | linear blend | **learned ranker** | learned − BM25 [95 % CI] | hit@1 / hit@5 / hit@10 (learned) |
|---|---:|---:|---:|---:|---:|---:|---|
| doc | 1 494 | 0.468 | 0.435 | 0.509 | **0.663** [0.642, 0.684] | +0.195 [+0.180, +0.213] | 0.549 / 0.823 / 0.873 |
| kw | 1 392 | 0.329 | 0.288 | 0.349 | **0.455** [0.433, 0.477] | +0.127 [+0.112, +0.141] | 0.343 / 0.609 / 0.687 |
| name | 1 129 | 0.712 | 0.630 | 0.727 | **0.930** [0.917, 0.942] | +0.218 [+0.196, +0.239] | 0.894 / 0.976 / 0.986 |
| id | 893 | 0.929 | 0.680 | 0.981 | **0.993** [0.988, 0.997] | +0.064 [+0.050, +0.077] | 0.988 / 0.998 / 0.998 |
| qid | 753 | 0.893 | 0.627 | 0.897 | **0.990** [0.985, 0.996] | +0.097 [+0.079, +0.115] | 0.983 / 0.997 / 1.000 |

## Variant B: first docstring sentence removed

| Query type | n | BM25 | **learned ranker** | learned − BM25 [95 % CI] |
|---|---:|---:|---:|---:|
| doc | 1 494 | 0.526 | **0.702** [0.683, 0.722] | +0.176 [+0.160, +0.192] |
| kw | 1 392 | 0.372 | **0.489** [0.467, 0.514] | +0.117 [+0.102, +0.131] |
| name | 1 129 | 0.692 | **0.928** [0.915, 0.941] | +0.236 [+0.216, +0.257] |
| id | 893 | 0.912 | **0.995** [0.991, 0.998] | +0.083 [+0.068, +0.097] |
| qid | 753 | 0.872 | **0.986** [0.980, 0.992] | +0.115 [+0.096, +0.133] |

## Other languages

Ten held-out repositories (`bench/groups.py` `CODE_TEST`): ripgrep and serde_json (Rust),
protobuf-go and client_golang (Go), jsoup and commons-io (Java), ws and lodash (JavaScript), RxJS
and PixiJS (TypeScript). Their queries are built the same way from doc comments
(`bench/make_bench_code.py`). Variant A, MRR@10 of the learned ranker against BM25; the last
column is the change from the previous, Python-only ranker (v7) on the same queries:

| Language | doc | kw | name | id | qid | v8 − v7, doc [95 % CI] |
|---|---|---|---|---|---|---:|
| Rust | 0.368 → **0.501** | 0.222 → **0.301** | 0.692 → **0.845** | 0.905 → **0.986** | 0.753 → **0.966** | +0.050 [+0.038, +0.065] |
| Go | 0.332 → **0.464** | 0.190 → **0.272** | 0.631 → **0.829** | 0.935 → **0.985** | (none) | +0.002 [−0.009, +0.012] |
| Java | 0.456 → **0.522** | 0.276 → **0.314** | 0.722 → **0.815** | 0.954 → **0.975** | 0.858 → **0.935** | +0.017 [+0.010, +0.024] |
| JavaScript | 0.406 → **0.581** | 0.289 → **0.391** | 0.603 → **0.868** | 0.883 → **0.997** | 0.912 → **1.000** | +0.008 [−0.006, +0.020] |
| TypeScript | 0.477 → **0.618** | 0.297 → **0.376** | 0.801 → **0.911** | 0.914 → **0.990** | 0.873 → **0.983** | +0.012 [+0.004, +0.021] |

Across both variants, six languages (Python included) and the query types each has, v8 is above
v7 in 56 of 58 cells. The other two are in JavaScript, variant B: name queries −0.001 [−0.016,
+0.013] and qualified names unchanged at 1.000. No cell's interval lies below zero. Per-cell numbers, hit@k and intervals: `compare_v8.log`.

A caveat for these rows. When the Python code ran this section it did not yet cut
chunks for Rust `mod` and `type` items, Java records or TypeScript abstract classes, which the
Rust engine does. It does now, and the two engines agree bit for bit on Rust sources as well
(`bench/parity.py crates/scrutate/src`, a CI step), but the rows above were not measured again:
for Rust, Java and TypeScript they describe an index without those chunks. The same holds for
the definitions both engines cut later, in the Rust, Go, JavaScript and TypeScript rows:
Rust `macro_rules!` and trait methods without a body, Go methods qualified by their receiver type
(`Type.Method`), JavaScript class field functions and `module.exports.X` / `exports.X`, and
TypeScript abstract methods, `declare function` and namespaces. The rows above predate them, and
two later changes as well: a Rust method of a generic type is qualified by the bare type
(`Stack.push`, where it was `Stack<T>.push`), a JavaScript or TypeScript object literal's
methods are qualified by the name that holds the literal (`api.get`), and constants are chunks
that ranked search never returns. Tokens of an all-ASCII text did not change when words of other scripts became tokens.

## What these numbers say and do not say

- The learned pipeline beats its own lexical layer on every query type with non-overlapping
  intervals, by the most where lexical search is weakest (natural-language and keyword queries in
  an undocumented code base).
- They cover six languages. The semantic model was trained on Python only; the ranker was fitted
  on Python and the five other languages above. Languages outside these six inherit the lexical
  layer's quality plus whatever the ranker and the code-word vectors generalise.
- They are not a comparison with other code-search products: no other tool in the benchmark set
  ranks by meaning, and public code-search benchmarks (e.g. natural-language-to-code retrieval
  sets) use different protocols. Absolute MRR values are not comparable across protocols.
- Hand-written queries over the `mcp` package (`handwritten_final.log`) show the same picture
  qualitatively (e.g. "initial handshake that negotiates protocol version": BM25 rank 5, Scrutate
  rank 2) but are too few to carry intervals.

## int8 vectors (schema 13)

Since schema 13 the index stores each semantic vector as int8 codes plus one float32 scale
(symmetric, per vector) instead of float16. The tables above use int8 vectors. The check below,
made with the previous ranker (v7) when the change went in, reruns the same protocol before and after the change on one freshly generated
benchmark (`bench/make_bench.py` on networkx 3.4.2, mcp 1.30.0 and rich 15.0.0, then
`bench/gen_features.py` and `bench/train_ranker.py eval` with the shipped models, not retrained).
The package versions differ from the v7 run, so n and the absolute values differ from the tables
above; only the difference matters here.

Learned ranker MRR@10, float16 → int8:

| Query type | n | variant A | variant B |
|---|---:|---:|---:|
| doc | 1 494 | 0.6521 → 0.6514 (−0.0007) | 0.6797 → 0.6808 (+0.0010) |
| kw | 1 392 | 0.4526 → 0.4518 (−0.0008) | 0.4891 → 0.4898 (+0.0008) |
| name | 1 129 | 0.9227 → 0.9223 (−0.0004) | 0.9233 → 0.9230 (−0.0003) |
| id | 893 | 0.9922 → 0.9922 (0) | 0.9924 → 0.9924 (0) |
| qid | 753 | 0.9861 → 0.9854 (−0.0007) | 0.9810 → 0.9810 (0) |

BM25, semantic only and linear blend move by at most 0.001 as well.

Vector recall on the reference engine's 121 760-chunk index of a copy of 10 site-packages (numpy,
pandas, scipy and others; the same tree as the size table below), 398 doc and kw queries:

- exact scan, int8 top-100 against float16 top-100: recall 0.9945 (top-10: 0.9915);
- default scan (48 of 698 clusters) against the float16 exact top-100: 0.8910 with int8,
  0.8901 with float16 (`bench/ivf_recall.py` at nprobe 48: 0.8922 and 0.8901);
- final top-10 of the whole pipeline: same first result for 376 of 398 queries, 96.1 % of the
  top-10 in common.

Index size, fresh index built by the Rust engine (`du`, MiB):

| Repository | chunks | float16 | int8 | vectors (`vec` + `vscale`) |
|---|---:|---:|---:|---:|
| networkx package | 8 054 | 18.8 | 14.9 (−21 %) | 7.9 → 4.0 |
| 10 site-packages | 161 208 | 343.0 | 265.5 (−23 %) | 157.4 → 79.9 |

Query latency (`scrutate search` on the large index, warm, 4 queries, 3 alternating rounds of
20 runs, median): 62.4 ms with float16, 61.5 ms with int8.

Parity (`bench/parity.py`, release build): bit-identical vectors, features and scores on the
networkx package (300 queries) and on a 119 358-chunk index that uses the clustered scan (the
`.py` files of the same 10 packages, 200 queries). To get there on the clustered path, both
engines sum the vector-scan dot products in float64 in one fixed order and round once to
float32; with float32 sums, two near-tied rows could swap places between the engines. Parity
holds end to end too, each engine on its own index, also on segments of 20 000 rows or more:
both engines build the same clusters bit for bit (go_protobuf, 41 377 chunks: 40/40), and parity
compares the centroids, cluster pointers, cluster order and stored vectors directly.
`bench/parity_repos.sh` covers the clustered path with go_protobuf.

Not shipped: compressing chunk text with a zstd dictionary trained on the repository's chunks.
On a 20 000-chunk sample of the large index, per-chunk zstd with a 110 KB trained dictionary is
19 % (level 3) to 30 % (level 19) smaller than the current zlib level 6; the whole text column is
41 MiB, so the index would shrink by 8 to 13 MiB (3 to 5 %), and by about 0.5 to 0.8 MiB on the
networkx package. That would add a C library to the engine and a third-party package to the
reference, a dictionary training pass before chunks can be compressed (1.6 s of CPU for 30 000
samples), and a dictionary to version in the index, for a few percent.

## Semantic model and ranker

- Semantic model (`models/semantic.npz`, 14.6 MB): a code-word model with two views per chunk
  (description: name + signature + docs; implementation: body), 256 dimensions, trained with an
  InfoNCE contrastive objective over (query, description), (query, implementation) and
  (description, implementation) pairs with 20 % word dropout, in numpy
  (`bench/train_semantic.py`). On the DEV packages its semantic-only doc MRR rose from 0.247
  (previous PLS-SVD model) to 0.427.
- Training data and its licences: the 37 Python packages listed in `bench/groups.py` (27 to
  pretrain the semantic model, 7 to tune and fit the ranker, 3 held out for the test). Their
  licences as declared on PyPI: BSD-2/3-Clause (pandas, scipy, scikit-learn, numpy, scikit-image,
  reportlab, pypdf, xlsxwriter, pygments, uvicorn, imageio, mammoth, protobuf, werkzeug, seaborn,
  joblib, networkx), MIT (spacy, mypy, fonttools, narwhals, thinc, srsly, anyio, markdown-it-py,
  pytest, pydantic, python-pptx, python-docx, openpyxl, mcp, rich), MIT-CMU (pillow), the
  Matplotlib licence, Apache-2.0 (playwright, cryptography as one option) and MPL-2.0 (pikepdf).
  The model files keep word statistics, vocabulary tables and projections, not source text.
- Ranker (`models/ranker.npz`, 86 KB, v8): LightGBM LambdaRank over 48 features, exported to
  arrays; numpy and LightGBM predictions agree to 5.3e-8; cross-validated on DEV packages (three
  folds, each language's repositories dealt across them), fitted once, evaluated once on TEST.
  Fitted on the 7 Python DEV packages and the 16 `CODE_DEV` repositories of `bench/groups.py`
  (tokio, clap, chrono, grpc-go, zap, cobra, commons-lang, gson, javapoet, guava, commander,
  axios, immutable-js, eslint, three.js, vue core; MIT, Apache-2.0, or both). Top features by
  gain: lexical ratio and rank, A-view semantic z-score, lexical z-score, name coverage, query
  length, mixed semantic z-score.
