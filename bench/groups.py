"""Disjoint repo groups used for pretraining, tuning and final testing.

Rule: a package appears in exactly one group. Packages that vendor another listed
package (e.g. `pip` bundles `rich`) are excluded from pretraining.
"""
import os
import site

SP = site.getsitepackages()[0]

# held out: only touched for the final report
TEST = ["networkx", "mcp", "rich"]

# sealed hold-out: never used to pretrain, tune, fit or test; evaluated once with the final ranker
# (bench/eval_sealed.sh). click 8.5.1 (BSD-3-Clause), httpx 0.28.1 (BSD-3-Clause), attrs 26.1.0 (MIT;
# `attr` holds the source, `attrs` only re-exports it)
SEALED = ["click", "httpx", "attr"]

# used to tune hyper-parameters and to train the learned ranker
DEV = ["pydantic", "pptx", "docx", "werkzeug", "seaborn", "openpyxl", "joblib"]

# used to pretrain the code-word semantic model (docstrings intact)
PRETRAIN = [
    "pandas", "scipy", "sklearn", "numpy", "matplotlib", "spacy", "skimage", "mypy",
    "fontTools", "reportlab", "narwhals", "pypdf", "PIL", "xlsxwriter", "thinc", "srsly",
    "cryptography", "anyio", "pygments", "markdown_it", "uvicorn", "_pytest", "imageio",
    "pikepdf", "mammoth", "playwright", "google",
]

# Other languages (make_bench_code.py): tag -> (language, git repository, tag, sub-paths that
# hold one copy of the library source, licence). Each repository is in exactly one group.
CODE = {
    "rs_tokio": ("rust", "https://github.com/tokio-rs/tokio", "tokio-1.54.1", ["tokio/src"], "MIT"),
    "rs_clap": ("rust", "https://github.com/clap-rs/clap", "v4.6.7", ["clap_builder/src", "clap_lex/src"],
                "MIT OR Apache-2.0"),
    "rs_chrono": ("rust", "https://github.com/chronotope/chrono", "v0.4.45", ["src"], "MIT OR Apache-2.0"),
    "rs_ripgrep": ("rust", "https://github.com/BurntSushi/ripgrep", "15.2.0", ["crates"], "Unlicense OR MIT"),
    "rs_serde_json": ("rust", "https://github.com/serde-rs/json", "v1.0.154", ["src"], "MIT OR Apache-2.0"),
    "go_grpc": ("go", "https://github.com/grpc/grpc-go", "v1.85.0", ["."], "Apache-2.0"),
    "go_zap": ("go", "https://github.com/uber-go/zap", "v1.28.0", ["."], "MIT"),
    "go_cobra": ("go", "https://github.com/spf13/cobra", "v1.10.2", ["."], "Apache-2.0"),
    "go_protobuf": ("go", "https://github.com/protocolbuffers/protobuf-go", "v1.36.12", ["."], "BSD-3-Clause"),
    "go_prometheus": ("go", "https://github.com/prometheus/client_golang", "v1.25.0", ["."], "Apache-2.0"),
    "java_commons_lang": ("java", "https://github.com/apache/commons-lang", "rel/commons-lang-3.21.0",
                          ["src/main/java"], "Apache-2.0"),
    "java_gson": ("java", "https://github.com/google/gson", "gson-parent-2.15.0", ["gson/src/main/java"],
                  "Apache-2.0"),
    "java_javapoet": ("java", "https://github.com/square/javapoet", "javapoet-1.13.0", ["src/main/java"],
                      "Apache-2.0"),
    "java_jsoup": ("java", "https://github.com/jhy/jsoup", "jsoup-1.24.1", ["src/main/java"], "MIT"),
    "java_commons_io": ("java", "https://github.com/apache/commons-io", "rel/commons-io-2.22.0",
                        ["src/main/java"], "Apache-2.0"),
    "js_commander": ("javascript", "https://github.com/tj/commander.js", "v15.1.0", ["lib", "index.js"], "MIT"),
    "js_axios": ("javascript", "https://github.com/axios/axios", "v1.21.1", ["lib"], "MIT"),
    "ts_immutable": ("typescript", "https://github.com/immutable-js/immutable-js", "v5.1.10", ["src"], "MIT"),
    "js_eslint": ("javascript", "https://github.com/eslint/eslint", "v10.14.0", ["lib"], "MIT"),
    "js_three": ("javascript", "https://github.com/mrdoob/three.js", "r187", ["src"], "MIT"),
    "ts_vue": ("typescript", "https://github.com/vuejs/core", "v3.5.45", ["packages"], "MIT"),
    "java_guava": ("java", "https://github.com/google/guava", "v33.8.0", ["guava/src"], "Apache-2.0"),
    "js_lodash": ("javascript", "https://github.com/lodash/lodash", "4.18.1-es", ["."], "MIT"),
    "ts_pixijs": ("typescript", "https://github.com/pixijs/pixijs", "v8.22.0", ["src"], "MIT"),
    "js_ws": ("javascript", "https://github.com/websockets/ws", "8.22.2", ["lib"], "MIT"),
    "ts_rxjs": ("typescript", "https://github.com/ReactiveX/rxjs", "7.8.2", ["src"], "Apache-2.0"),
    "rs_rayon": ("rust", "https://github.com/rayon-rs/rayon", "v1.12.0", ["src", "rayon-core/src"],
                 "MIT OR Apache-2.0"),
    "go_gin": ("go", "https://github.com/gin-gonic/gin", "v1.12.0", ["."], "MIT"),
    "java_commons_collections": ("java", "https://github.com/apache/commons-collections",
                                 "rel/commons-collections-4.6.0", ["src/main/java"], "Apache-2.0"),
    "js_videojs": ("javascript", "https://github.com/videojs/video.js", "v8.24.2", ["src/js"], "Apache-2.0"),
    "ts_typeorm": ("typescript", "https://github.com/typeorm/typeorm", "1.1.2", ["packages/typeorm/src"], "MIT"),
}
CODE_DEV = ["rs_tokio", "rs_clap", "rs_chrono", "go_grpc", "go_zap", "go_cobra",
            "java_commons_lang", "java_gson", "java_javapoet", "java_guava",
            "js_commander", "js_axios", "ts_immutable", "js_eslint", "js_three", "ts_vue"]
CODE_TEST = ["rs_ripgrep", "rs_serde_json", "go_protobuf", "go_prometheus", "java_jsoup", "java_commons_io",
             "js_ws", "js_lodash", "ts_rxjs", "ts_pixijs"]
CODE_SEALED = ["rs_rayon", "go_gin", "java_commons_collections", "js_videojs", "ts_typeorm"]

_py = [PRETRAIN, DEV, TEST, SEALED]
_code = [CODE_DEV, CODE_TEST, CODE_SEALED]
assert len({p for g in _py for p in g}) == sum(len(g) for g in _py), "a package is in two groups"
assert len({p for g in _code for p in g}) == sum(len(g) for g in _code) == len(CODE), "a repository is in two groups or none"


def language(pkg: str) -> str:
    """Language of a benchmark package (Python packages have no prefix)."""
    return CODE[pkg][0] if pkg in CODE else "python"
assert "pip" not in PRETRAIN and "setuptools" not in PRETRAIN  # they vendor other packages

# scratch space for generated benchmarks, features and logs (not part of the repository)
WORK = os.environ.get("SCRUTATE_WORK", os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "work"))
BENCH = os.path.join(WORK, "bench")
CODE_SRC = os.path.join(WORK, "code")  # read-only clones of the CODE repositories


def is_test_path(path: str) -> bool:
    parts = path.lower().replace("\\", "/").split("/")
    return any(p in ("test", "tests", "testing", "conftest.py") or p.startswith("test_")
               or p.endswith("_test.py") for p in parts)
