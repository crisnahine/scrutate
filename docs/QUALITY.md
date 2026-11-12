<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-quality.svg" alt="Ranking quality. How good are ranked answers on code it was not fitted on?" width="880">
</p>

# Ranking quality (Engine B)

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Benchmarks](BENCHMARKS.md) &nbsp;·&nbsp; Next: [Test results](TEST_RESULTS.md)

**Contents**: [Protocol](#protocol) &nbsp;·&nbsp; [Python, variant A: every docstring removed (MRR@10)](#python-variant-a-every-docstring-removed-mrr10) &nbsp;·&nbsp; [Python, variant B: first docstring sentence removed](#python-variant-b-first-docstring-sentence-removed) &nbsp;·&nbsp; [Other languages](#other-languages) &nbsp;·&nbsp; [What weighing type answers cost](#what-weighing-type-answers-cost) &nbsp;·&nbsp; [The sealed group](#the-sealed-group) &nbsp;·&nbsp; [What these numbers say and do not say](#what-these-numbers-say-and-do-not-say) &nbsp;·&nbsp; [int8 vectors (schema 13)](#int8-vectors-schema-13) &nbsp;·&nbsp; [Semantic model and ranker](#semantic-model-and-ranker) &nbsp;·&nbsp; [Reproduce](#reproduce)

<!-- End of head. -->

How good are the answers, as opposed to how fast? Numbers below are produced by
`bench/retrain_ranker.sh` (its `eval` and `compare` stages; logs `eval.log`, `eval_python.log` and
`compare.log` of run `f3`, 2026-11-12) on packages that were never used to train the semantic
model or to fit the ranker.

Two sets are held out, and they do not carry the same weight:

- **TEST** (three Python packages and ten repositories in five other languages) was in view while
  this ranker was chosen. Every fit made on the way was read on it: four training weights for
  type answers, then a fit after each later change to the engine. The rule that accepts a new
  ranker was changed twice after a fit failed it. The tables show how the shipped ranker does on
  code it was not fitted on, but the choices were made looking at them, so they are not an
  untouched estimate.
- **SEALED** (three more Python packages and five more repositories) was never used to pretrain,
  tune, fit or test, and no ranker has been run on it. It is evaluated once, with the ranker that
  ships, after the last change to the engine (see "The sealed group").

The Rust engine produces bit-identical candidates, features and scores to the Python code that ran
this evaluation, so the numbers hold for the shipped binary. `bench/parity.py` checks it with no
tolerance: any difference in the chunk tables, a feature, a score, a top 10 or the end-to-end
result fails, and so does a run that cannot make as many queries as asked. Its queries are built
from the corpus's documented functions by the benchmark's rules: 300 on networkx and 50 on the
engine's own sources (`crates/scrutate/src`, a CI step). `bench/parity_repos.sh` runs it with 40
queries on 21 real repositories of the six languages the reference has a grammar for; parity
indexes only a repository's files in those six languages.

Parity says the two engines agree, not that ranking is good. `bench/quality.py` measures that on
its own, in CI and in the release gates: it indexes a frozen copy of this repository's code
(`crates/scrutate/src`, `vendor/ignore/src`, `bench/*.py`, `tools/*.py` with every doc comment
and docstring removed, variant A below, kept in `bench/quality/corpus.tar.gz`), runs the 1,183
committed queries of `bench/quality/queries.json` (the five types below, at most 150 of each
type and target kind, made from those doc comments) through the binary, and fails when MRR@10
or hit@5 is below the floor in `bench/quality/floor.json`. The copy and the queries are written
together by `bench/quality.py --make`, so an edit to the repository's code does not move the
measure; only the engine does. The floor is the measured value less 0.02; the file records the
measured values (MRR@10 0.5873, hit@5 0.6636 on 2026-11-13), the commit, the vector model's
signature and the ranker's hash. The same queries ranked by the linear blend alone (no ranker)
score MRR@10 0.4977 and hit@5 0.6323, below both floors. A run takes about 8 s.

## Protocol

- A query is generated from a real definition; the definition it came from is the one correct
  answer.
- The answer is a function or method, or a documented type (class, struct, enum, trait,
  interface, type alias). Type queries are drawn after a package's function queries from their
  own random stream, so adding them did not move the function queries. An impl block or a
  constructor does not answer a type query. Every table gives function answers and type answers
  apart.
- The indexed copy has that definition's documentation removed (**variant A**: every docstring in
  the package removed, an undocumented code base; **variant B**: only the first sentence of each
  docstring removed), so the answer text is not sitting in the index.
- Query types: **doc** = the first documentation sentence written as a natural-language request;
  **kw** = three content words sampled from it; **name** = the name as words (`remove tool`);
  **id** = the bare identifier (`remove_tool`); **qid** = the qualified name
  (`Server.remove_tool`).
- **MRR@10**: mean of 1/rank of the correct result (0 when not in the top 10). **hit@k**: share of
  queries with the correct result in the top k. Brackets: 95 % bootstrap intervals over queries.
- TEST: networkx 3.4.2, mcp 1.30.0, rich 15.0.0 (Python), and ten repositories in other languages
  (below). Compared systems run on the **same candidate pool, tokenizer, stemming and field
  weights**: *BM25* is Scrutate's own lexical layer alone; *semantic only* ranks by the vector
  similarity alone; *linear blend* is the hand-set fallback used when no ranker is present;
  *learned* is the shipped pipeline.

## Python, variant A: every docstring removed (MRR@10)

Function answers:

| Query type | n | BM25 | semantic only | linear blend | **learned ranker** | learned − BM25 [95 % CI] | hit@1 / hit@5 / hit@10 (learned) |
|---|---:|---:|---:|---:|---:|---:|---|
| doc | 1 494 | 0.463 | 0.435 | 0.508 | **0.635** [0.614, 0.653] | +0.172 [+0.156, +0.188] | 0.513 / 0.798 / 0.863 |
| kw | 1 392 | 0.322 | 0.285 | 0.344 | **0.412** [0.390, 0.435] | +0.090 [+0.075, +0.103] | 0.295 / 0.572 / 0.663 |
| name | 1 129 | 0.714 | 0.629 | 0.726 | **0.916** [0.902, 0.928] | +0.201 [+0.180, +0.223] | 0.873 / 0.967 / 0.979 |
| id | 893 | 0.927 | 0.704 | 0.985 | **1.000** [1.000, 1.000] | +0.073 [+0.058, +0.087] | 1.000 / 1.000 / 1.000 |
| qid | 753 | 0.885 | 0.616 | 0.894 | **0.996** [0.991, 0.999] | +0.110 [+0.091, +0.128] | 0.992 / 0.999 / 1.000 |

Type answers:

| Query type | n | BM25 | semantic only | linear blend | **learned ranker** | learned − BM25 [95 % CI] | hit@1 / hit@5 / hit@10 (learned) |
|---|---:|---:|---:|---:|---:|---:|---|
| doc | 457 | 0.280 | 0.274 | 0.366 | **0.558** [0.522, 0.594] | +0.278 [+0.244, +0.310] | 0.411 / 0.751 / 0.827 |
| kw | 437 | 0.214 | 0.230 | 0.284 | **0.407** [0.369, 0.443] | +0.192 [+0.161, +0.227] | 0.288 / 0.572 / 0.670 |
| name | 374 | 0.730 | 0.695 | 0.746 | **0.947** [0.929, 0.964] | +0.217 [+0.185, +0.255] | 0.909 / 0.995 / 0.995 |
| id | 437 | 0.831 | 0.665 | 0.955 | **1.000** [1.000, 1.000] | +0.169 [+0.138, +0.199] | 1.000 / 1.000 / 1.000 |
| qid | 10 | 0.684 | 0.428 | 0.850 | **1.000** [1.000, 1.000] | +0.316 [+0.100, +0.526] | 1.000 / 1.000 / 1.000 |

## Python, variant B: first docstring sentence removed

Function answers:

| Query type | n | BM25 | **learned ranker** | learned − BM25 [95 % CI] |
|---|---:|---:|---:|---:|
| doc | 1 494 | 0.522 | **0.666** [0.647, 0.685] | +0.144 [+0.127, +0.160] |
| kw | 1 392 | 0.371 | **0.451** [0.429, 0.474] | +0.081 [+0.067, +0.096] |
| name | 1 129 | 0.693 | **0.917** [0.904, 0.930] | +0.223 [+0.203, +0.244] |
| id | 893 | 0.911 | **1.000** [1.000, 1.000] | +0.089 [+0.073, +0.105] |
| qid | 753 | 0.867 | **0.996** [0.991, 0.999] | +0.128 [+0.108, +0.147] |

Type answers:

| Query type | n | BM25 | **learned ranker** | learned − BM25 [95 % CI] |
|---|---:|---:|---:|---:|
| doc | 457 | 0.285 | **0.550** [0.514, 0.584] | +0.265 [+0.232, +0.297] |
| kw | 437 | 0.243 | **0.434** [0.394, 0.471] | +0.190 [+0.161, +0.222] |
| name | 374 | 0.701 | **0.933** [0.912, 0.951] | +0.232 [+0.196, +0.268] |
| id | 437 | 0.813 | **1.000** [1.000, 1.000] | +0.187 [+0.155, +0.219] |
| qid | 10 | 0.662 | **1.000** [1.000, 1.000] | +0.338 [+0.117, +0.543] |

## Other languages

Ten held-out repositories (`bench/groups.py` `CODE_TEST`): ripgrep and serde_json (Rust),
protobuf-go and client_golang (Go), jsoup and commons-io (Java), ws and lodash (JavaScript), RxJS
and PixiJS (TypeScript). Their queries are built the same way from doc comments
(`bench/make_bench_code.py`). Variant A, MRR@10, BM25 → learned ranker.

Function answers:

| Language | doc | kw | name | id | qid |
|---|---|---|---|---|---|
| Rust | 0.354 → **0.459** | 0.215 → **0.246** | 0.704 → **0.813** | 0.929 → **0.996** | 0.755 → **0.989** |
| Go | 0.315 → **0.419** | 0.162 → **0.211** | 0.625 → **0.800** | 0.926 → **1.000** | 0.877 → **0.965** |
| Java | 0.458 → **0.488** | 0.278 → **0.279** | 0.731 → **0.813** | 0.961 → **0.982** | 0.841 → **0.916** |
| JavaScript | 0.416 → **0.608** | 0.267 → **0.367** | 0.632 → **0.871** | 0.864 → **1.000** | 0.903 → **1.000** |
| TypeScript | 0.469 → **0.570** | 0.269 → **0.297** | 0.801 → **0.903** | 0.920 → **1.000** | 0.848 → **0.989** |

Type answers (a cell with fewer than 20 queries gives the count and no number):

| Language | doc | kw | name | id | qid |
|---|---|---|---|---|---|
| Rust | 0.190 → **0.244** | 0.136 → **0.168** | 0.688 → **0.812** | 0.725 → **0.937** | (5 queries) |
| Go | 0.224 → **0.339** | 0.145 → **0.228** | 0.587 → **0.762** | 0.736 → **0.994** | (none) |
| Java | 0.216 → **0.457** | 0.130 → **0.315** | 0.298 → **0.914** | 0.466 → **0.979** | 0.441 → **0.975** |
| JavaScript | (7 queries) | (5 queries) | (5 queries) | (7 queries) | (none) |
| TypeScript | 0.316 → **0.600** | 0.204 → **0.380** | 0.688 → **0.976** | 0.820 → **1.000** | (none) |

Java keyword queries with a function answer are the one row where the learned ranker does not
clear BM25: 0.279 against 0.278. Per-cell numbers, hit@k and intervals for every language and
both variants: `compare.log`.

## What weighing type answers cost

The ranker before this one was fitted on function answers alone. It ranked a type first for
almost no documentation query (doc with a type answer: 0.017). This ranker is fitted on function
and type answers with the same weight. All languages, the previous ranker against this one on
the same queries:

| Variant | row | n | before | now | now − before [95 % CI] |
|---|---|---:|---:|---:|---:|
| A | doc | 7 024 | 0.564 | 0.534 | -0.031 [-0.034, -0.027] |
| A | kw | 5 333 | 0.347 | 0.312 | -0.035 [-0.040, -0.032] |
| A | name | 4 277 | 0.869 | 0.857 | -0.012 [-0.016, -0.008] |
| A | id | 3 245 | 0.990 | 0.996 | +0.006 [+0.004, +0.008] |
| A | qid | 2 516 | 0.967 | 0.971 | +0.004 [+0.001, +0.007] |
| A | doc:type | 2 145 | 0.017 | 0.478 | +0.462 [+0.445, +0.478] |
| A | kw:type | 2 032 | 0.008 | 0.326 | +0.318 [+0.301, +0.336] |
| A | name:type | 1 678 | 0.404 | 0.909 | +0.504 [+0.487, +0.521] |
| A | id:type | 1 613 | 0.637 | 0.987 | +0.350 [+0.333, +0.366] |
| A | qid:type | 114 | 0.263 | 0.969 | +0.707 [+0.668, +0.744] |
| A | every query | 29 977 | 0.566 | 0.656 | +0.090 [+0.087, +0.093] |
| B | doc | 7 024 | 0.623 | 0.591 | -0.032 [-0.036, -0.029] |
| B | kw | 5 333 | 0.405 | 0.370 | -0.035 [-0.039, -0.031] |
| B | name | 4 277 | 0.871 | 0.861 | -0.010 [-0.014, -0.005] |
| B | id | 3 245 | 0.991 | 0.996 | +0.005 [+0.004, +0.007] |
| B | qid | 2 516 | 0.967 | 0.972 | +0.006 [+0.003, +0.009] |
| B | doc:type | 2 145 | 0.023 | 0.508 | +0.485 [+0.469, +0.502] |
| B | kw:type | 2 032 | 0.012 | 0.366 | +0.354 [+0.336, +0.372] |
| B | name:type | 1 678 | 0.485 | 0.915 | +0.431 [+0.414, +0.448] |
| B | id:type | 1 613 | 0.678 | 0.991 | +0.314 [+0.297, +0.330] |
| B | qid:type | 114 | 0.276 | 0.982 | +0.707 [+0.667, +0.747] |
| B | every query | 29 977 | 0.598 | 0.686 | +0.088 [+0.085, +0.091] |

Function answers lost 0.031 on doc and 0.035 on kw queries (variant A), and type answers went
from almost never found to found about as well as functions. Over every query the mean rose from
0.566 to 0.656.

Where the function rows lose: in cross-validation on the DEV packages, of 33 308 function doc and
kw queries the function-only weighting ranks the answer first and the equal weighting does not in
1 771, and the reverse holds in 667. What stands above the answer then is another type in 44 % of
cases, another function in 25 %, the class or impl that holds the answer in 22 %, and a sibling
method in 8 %. 88 % of those other types share a word with the query in their name. The cost is a
type whose name matches the query, which nothing in the candidate set tells apart from a wanted
function. Features for the holder of a method and for its members were tried and lifted no
function row by 0.005 in cross-validation, so they are not in the engine.

The weight was compared at 0.1, 0.3, 0.5 and 1.0 on TEST (variant A, all languages): doc with a
function answer 0.564 / 0.558 / 0.547 / 0.533, doc with a type answer 0.240 / 0.360 / 0.417 /
0.478, the mean over every query 0.632 / 0.649 / 0.653 / 0.655. At 0.1 the learned ranker is
under BM25 on doc and kw queries with a type answer. Equal weight has the highest mean over
every query, on TEST and in cross-validation on DEV (0.616 at 0.1, 0.636 at 0.3, 0.649 at 1.0),
and it counts every query the same.

## The sealed group

`bench/groups.py` `SEALED` and `CODE_SEALED`: click 8.5.1, httpx 0.28.1 and attrs 26.1.0
(Python), rayon v1.12.0 (Rust), gin v1.12.0 (Go), commons-collections 4.6.0 (Java), video.js
v8.24.2 (JavaScript) and TypeORM 1.1.2 (TypeScript). None of them was used to pretrain the
semantic model, to tune, to fit or to test, and the semantic model's training refuses them by
name. `bench/eval_sealed.sh` evaluates the ranker in `models/ranker.npz` on the group; it records
each evaluation in `sealed_evaluated.json` (ranker SHA-256, commit, date) and refuses a second
ranker unless `FORCE_SEALED=1` is set, which it records too.

The group has not been evaluated at this commit. The one evaluation is made on the ranker that
ships, after the last change to the engine, and its table goes here.

## What these numbers say and do not say

- On TEST the learned pipeline beats its own lexical layer on every row of 50 queries or more
  except Java keyword queries, where the two tie. The gain is largest where lexical search is
  weakest: natural-language and keyword queries in an undocumented code base, and type answers.
- They cover six languages. The semantic model was trained on Python only; the ranker was fitted
  on Python and the five other languages above. Languages outside these six inherit the lexical
  layer's quality plus whatever the ranker and the code-word vectors generalise.
- They are not a comparison with other code-search products: no other tool in the benchmark set
  ranks by meaning, and public code-search benchmarks (e.g. natural-language-to-code retrieval
  sets) use different protocols. Absolute MRR values are not comparable across protocols.
- A constant is found through the window that holds its line, never as a result of its own, so no
  row measures constants.

## int8 vectors (schema 13)

Since schema 13 the index stores each semantic vector as int8 codes plus one float32 scale
(symmetric, per vector) instead of float16. The tables above use int8 vectors. The check below,
made with an earlier ranker (v7) when the change went in, reruns the same protocol before and after the change on one freshly generated
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
  0.8901 with float16 (`bench/ivf_recall.py` at nprobe 48: 0.8922 and 0.8901; the script ran on
  the reference engine and was removed with it, and commit `9e70ae7` is the last that has it);
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
  The model files keep word statistics, vocabulary tables and projections, not source text. The
  sealed group is read for evaluation only: click and httpx (BSD-3-Clause), attrs (MIT), rayon
  (MIT or Apache-2.0), gin (MIT), commons-collections (Apache-2.0), video.js (Apache-2.0),
  TypeORM (MIT).
- Ranker (`models/ranker.npz`, 86 KB): LightGBM LambdaRank over 48 features, exported to arrays;
  numpy and LightGBM predictions agree to 3.5e-8. Fitted on the 7 Python DEV packages and the 16
  `CODE_DEV` repositories of `bench/groups.py` (tokio, clap, chrono, grpc-go, zap, cobra,
  commons-lang, gson, javapoet, guava, commander, axios, immutable-js, eslint, three.js, vue
  core; MIT, Apache-2.0, or both), function and type answers weighted alike (`TYPE_WEIGHT=1`).
  `bench/retrain_ranker.sh` installs a new ranker only when on TEST no row of 50 queries or more
  falls below BM25 beyond its interval and the mean over every query of a variant is not below
  the previous ranker's. Top features by gain: lexical ratio, rank and z-score, name coverage,
  A-view semantic z-score, query length, name ratio, the name spelled as typed.

## Reproduce

```sh
# the features come from target/release/scrutate, which retrain_ranker.sh builds first
# (SCRUTATE_BIN names another build);
# a virtual environment with numpy, lightgbm, scikit-learn, the reference engine's dependencies
# and the DEV, TEST and SEALED packages at the versions named in the scripts' headers
export SCRUTATE_WORK=/some/scratch SCRUTATE_VENV=/path/to/venv THREADS=4
bash bench/retrain_ranker.sh   # benchmark, features, fit on DEV, old and new ranker on TEST
bash bench/eval_sealed.sh      # the sealed group, once per ranker
```

`retrain_ranker.sh` writes `models/ranker.npz` only when the new ranker passes the rule above, and
the binary embeds that file, so rebuild it afterwards. `eval_sealed.sh` never changes the ranker.
