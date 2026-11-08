"""Disjoint repo groups used for pretraining, tuning and final testing.

Rule: a package appears in exactly one group. Packages that vendor another listed
package (e.g. `pip` bundles `rich`) are excluded from pretraining.
"""
import os
import site

SP = site.getsitepackages()[0]

# held out: only touched for the final report
TEST = ["networkx", "mcp", "rich"]

# used to tune hyper-parameters and to train the learned ranker
DEV = ["pydantic", "pptx", "docx", "werkzeug", "seaborn", "openpyxl", "joblib"]

# used to pretrain the code-word semantic model (docstrings intact)
PRETRAIN = [
    "pandas", "scipy", "sklearn", "numpy", "matplotlib", "spacy", "skimage", "mypy",
    "fontTools", "reportlab", "narwhals", "pypdf", "PIL", "xlsxwriter", "thinc", "srsly",
    "cryptography", "anyio", "pygments", "markdown_it", "uvicorn", "_pytest", "imageio",
    "pikepdf", "mammoth", "playwright", "google",
]

assert not (set(TEST) & set(DEV) | set(TEST) & set(PRETRAIN) | set(DEV) & set(PRETRAIN))
assert "pip" not in PRETRAIN and "setuptools" not in PRETRAIN  # they vendor other packages

# scratch space for generated benchmarks, features and logs (not part of the repository)
WORK = os.environ.get("SCRUTATE_WORK", os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "work"))
BENCH = os.path.join(WORK, "bench")


def is_test_path(path: str) -> bool:
    parts = path.lower().replace("\\", "/").split("/")
    return any(p in ("test", "tests", "testing", "conftest.py") or p.startswith("test_")
               or p.endswith("_test.py") for p in parts)
