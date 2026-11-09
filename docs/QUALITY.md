# Ranking quality (Engine B)

How good are the answers, as opposed to how fast? Numbers below are produced by
`bench/train_ranker.py eval v7` (log: `test_eval_v7.log`, 2026-11-08) on packages that were never
used to train the semantic model, fit the ranker or choose hyper-parameters. The Rust engine
produces bit-identical candidates, features and scores to the Python code that ran this evaluation
(`bench/parity.py`), so the numbers hold for the shipped binary.

## Protocol

- A query is generated from a real function; the function it came from is the one correct answer.
- The indexed copy has that function's docstring removed (**variant A**: every docstring in the
  package removed — an undocumented code base; **variant B**: only the first sentence of each
  docstring removed), so the answer text is not sitting in the index.
- Query types: **doc** = the first docstring sentence written as a natural-language request;
  **kw** = three content words sampled from it; **name** = the function name as words
  (`remove tool`); **id** = the bare identifier (`remove_tool`); **qid** = the qualified name
  (`Server.remove_tool`).
- **MRR@10**: mean of 1/rank of the correct result (0 when not in the top 10). **hit@k**: share of
  queries with the correct result in the top k. Brackets: 95 % bootstrap intervals over queries.
- Held-out packages: networkx, mcp, rich (Python). Compared systems run on the **same candidate
  pool, tokenizer, stemming and field weights**: *BM25* is Scrutate's own lexical layer alone;
  *semantic only* ranks by the vector similarity alone; *linear blend* is the hand-set fallback
  used when no ranker is present; *learned* is the shipped pipeline.

## Variant A — every docstring removed (MRR@10, n queries)

| Query type | n | BM25 | semantic only | linear blend | **learned ranker** | learned − BM25 [95 % CI] | hit@1 / hit@5 / hit@10 (learned) |
|---|---:|---:|---:|---:|---:|---:|---|
| doc | 1 695 | 0.448 | 0.422 | 0.497 | **0.632** [0.613, 0.651] | +0.184 [+0.170, +0.198] | 0.509 / 0.791 / 0.869 |
| kw | 1 604 | 0.302 | 0.261 | 0.321 | **0.420** [0.399, 0.440] | +0.118 [+0.104, +0.132] | 0.305 / 0.570 / 0.661 |
| name | 1 274 | 0.746 | 0.661 | 0.753 | **0.930** [0.918, 0.942] | +0.184 [+0.165, +0.203] | 0.898 / 0.972 / 0.986 |
| id | 900 | 0.923 | 0.698 | 0.984 | **0.989** [0.983, 0.994] | +0.066 [+0.052, +0.080] | 0.983 / 0.996 / 0.996 |
| qid | 754 | 0.886 | 0.615 | 0.894 | **0.980** [0.972, 0.987] | +0.094 [+0.077, +0.113] | 0.963 / 0.999 / 1.000 |

## Variant B — first docstring sentence removed

| Query type | n | BM25 | **learned ranker** | learned − BM25 [95 % CI] |
|---|---:|---:|---:|---:|
| doc | 1 695 | 0.505 | **0.673** [0.655, 0.691] | +0.168 [+0.154, +0.182] |
| kw | 1 604 | 0.349 | **0.460** [0.438, 0.480] | +0.111 [+0.097, +0.125] |
| name | 1 274 | 0.723 | **0.930** [0.917, 0.941] | +0.207 [+0.187, +0.226] |
| id | 900 | 0.912 | **0.988** [0.981, 0.994] | +0.076 [+0.062, +0.092] |
| qid | 754 | 0.866 | **0.975** [0.967, 0.983] | +0.110 [+0.091, +0.127] |

## What these numbers say and do not say

- The learned pipeline beats its own lexical layer on every query type with non-overlapping
  intervals, by the most where lexical search is weakest (natural-language and keyword queries in
  an undocumented code base).
- They are **Python** results. The ranker's training data is Python-heavy; a JavaScript evaluation
  set (lodash-es, commander, ws, antlr4 — a language the models never saw) exists in the harness
  and was run on earlier model versions; it is not reproduced here for the shipped v7 model and
  so no JavaScript quality claim is made for 0.1.0. Other languages inherit the lexical layer's
  quality plus whatever the code-word vectors generalise.
- They are not a comparison with other code-search products: no other tool in the benchmark set
  ranks by meaning, and public code-search benchmarks (e.g. natural-language-to-code retrieval
  sets) use different protocols. Absolute MRR values are not comparable across protocols.
- Hand-written queries over the `mcp` package (`handwritten_final.log`) show the same picture
  qualitatively (e.g. "initial handshake that negotiates protocol version": BM25 rank 5, Scrutate
  rank 2) but are too few to carry intervals.

## int8 vectors (schema 13)

Since schema 13 the index stores each semantic vector as int8 codes plus one float32 scale
(symmetric, per vector) instead of float16. The tables above were measured with float16 vectors.
The check below reruns the same protocol before and after the change on one freshly generated
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
engines now sum the vector-scan dot products in float64 and round once to float32; with float32
sums, two near-tied rows could swap places between the engines.

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
- Ranker (`models/ranker.npz`, 87 KB): LightGBM LambdaRank over 47 features, exported to arrays;
  numpy and LightGBM predictions agree to 4.3e-8; cross-validated on DEV packages, fitted once,
  evaluated once on TEST. Top features by gain: lexical ratio and rank, name coverage, mixed
  semantic z-score, A-view semantic z-score, query length, lexical z-score, chunk kind.
