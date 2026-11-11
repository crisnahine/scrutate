"""Text that is not Latin script: words of every script, names compared in NFC, long names."""
from scrutate_ref.index import Index
from scrutate_ref.search import Searcher
from scrutate_ref.text import name_key, query_terms, tokenize2

SCRIPTS = '''def calc_total(items):
    """计算订单总价，包括税费和运费。"""
    return 1


def 関数():
    """注文の合計金額を計算する"""
    return 2


def выручка():
    """Вычисляет общую выручку за период"""
    return 3


def korean_total():
    """주문 총액을 계산합니다"""
    return 4


def arabic_total():
    """يحسب إجمالي الطلب"""
    return 5


def parse_config(path):
    """Read the configuration file."""
    return path
'''

CASES = [
    ("计算订单总价", "calc_total"), ("订单", "calc_total"), ("関数", "関数"), ("注文の合計金額", "関数"),
    ("合計", "関数"), ("выручка", "выручка"), ("общую выручку", "выручка"), ("Вычисляет", "выручка"),
    ("주문 총액", "korean_total"), ("계산합니다", "korean_total"), ("إجمالي الطلب", "arabic_total"),
]


def _searcher(tmp_path, files):
    for rel, text in files.items():
        (tmp_path / rel).write_text(text, encoding="utf8")
    ix = Index(str(tmp_path))
    ix.update()
    return Searcher(ix)


def test_queries_in_every_script_find_the_function(tmp_path):
    s = _searcher(tmp_path, {"m.py": SCRIPTS})
    missed = [(q, [h.name for h in s.search(q, k=1)]) for q, want in CASES
              if [h.name for h in s.search(q, k=1)] != [want]]
    assert missed == []
    assert s.search("read configuration file", k=1)[0].name == "parse_config"


def test_ascii_tokens_are_unchanged():
    assert tokenize2("def parseHTTPResponse_v2(self): 2fa refreshing tokens") == (
        ["pars", "http", "respons", "fa", "refresh", "token"], ["parsehttpresponsev"])
    assert query_terms("where is the oauth token refreshed?") == ["oauth", "token", "refresh"]


def test_unspaced_scripts_are_bigrams_and_spaced_scripts_are_words():
    assert tokenize2("订单总价")[0] == ["订单", "单总", "总价"]
    assert tokenize2("税")[0] == ["税"]
    assert tokenize2("Вычисляет ОБЩУЮ")[0] == ["вычисляет", "общую"]
    assert tokenize2("parsing_выручки")[0] == ["pars", "выручки"]
    assert tokenize2("x関数y")[0] == ["関数"]
    assert query_terms("合計 合計") == ["合計"]
    # Thai, Lao, Myanmar, Khmer, Javanese: no spaces between words either
    for run in ["ภาษาไทย", "ພາສາລາວ", "မြန်မာစာ", "ភាសាខ្មែរ", "ꦧꦱꦗꦮ"]:
        assert tokenize2(run)[0] == [run[i:i + 2] for i in range(len(run) - 1)], run
    # Tibetan marks its syllables: the tsheg between them ends a run
    assert tokenize2("བོད་སྐད")[0] == ["བོ", "ོད", "སྐ", "ྐད"]


def test_names_compare_in_nfc(tmp_path):
    nfc, nfd = "café", "café"
    assert name_key(nfd) == name_key(nfc)
    assert name_key("ΛΌΓΟΣ") == name_key("λόγος")
    assert name_key("Send_Request") == "sendrequest"
    s = _searcher(tmp_path, {"a.py": f"def {nfc}():\n    return 1\n", "b.py": f"def {nfd}_x():\n    return 2\n"})
    for q in (nfc, nfd):
        assert [h.path for h in s.find_symbol(q)] == ["a.py"]
        assert [h.path for h in s.find_symbol(q + "_x")] == ["b.py"]
    assert s.search(nfd, k=1)[0].name == nfc


def test_very_long_names_are_found_by_their_exact_name(tmp_path):
    names = ["f" + "a" * (n - 1) for n in (200, 255, 256, 300, 600)]
    s = _searcher(tmp_path, {"m.py": "".join(f"def {n}():\n    return 1\n\n\n" for n in names)})
    for n in names:
        assert [h.name for h in s.find_symbol(n)] == [n]
        assert s.search(n, k=1)[0].name == n
