"""Text that is not Latin script: words of every script, names compared in NFC, long names."""
from scrutate_ref.index import Index
from scrutate_ref.search import Searcher
import hashlib

from scrutate_ref.text import han_fold_path, name_key, query_terms, tokenize2

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
    # Unihan gives the Japanese 関 a simplified form too (U+2CBA6)
    assert tokenize2("x関数y")[0] == ["\U0002CBA6数"]
    assert query_terms("合計 合計") == ["合计"]
    # Thai, Lao, Myanmar, Khmer, Javanese: no spaces between words either
    for run in ["ภาษาไทย", "ພາສາລາວ", "မြန်မာစာ", "ភាសាខ្មែរ", "ꦧꦱꦗꦮ"]:
        assert tokenize2(run)[0] == [run[i:i + 2] for i in range(len(run) - 1)], run
    # Tibetan marks its syllables: the tsheg between them ends a run
    assert tokenize2("བོད་སྐད")[0] == ["བོ", "ོད", "སྐ", "ྐད"]


def test_traditional_and_simplified_han_give_the_same_tokens():
    # a regenerated table must update this hash, here and the numbers in the Rust test
    with open(han_fold_path(), "rb") as f:
        assert hashlib.sha256(f.read()).hexdigest() == "483e5590b59db72686a992fc5c5ed2e94e5ccd37fb01f558785a7e103012dcd0"
    assert tokenize2("資料庫") == tokenize2("资料库")
    assert tokenize2("資料庫")[0] == ["资料", "料库"]
    assert tokenize2("庫")[0] == ["库"]
    assert tokenize2("資料库")[0] == ["资料", "料库"]
    # several simplified variants: the first that is not the character itself
    assert tokenize2("乾")[0] == ["干"]
    assert tokenize2("噁")[0] == ["恶"]
    assert tokenize2("データ庫の")[0] == ["デー", "ータ", "タ库", "库の"]
    assert tokenize2("ひらがな")[0] == ["ひら", "らが", "がな"]
    assert query_terms("讀取 资料库") == query_terms("读取 資料庫")
    assert name_key("資料庫") == "資料庫"


HAN_FORMS = '''def load_settings(path):
    """連接資料庫並讀取使用者設定"""
    return path


def notify_admin(msg):
    """发送电子邮件通知给管理员"""
    return msg


def parse_config(path):
    """Read the configuration file."""
    return path
'''


def test_a_query_in_either_han_form_finds_text_in_the_other(tmp_path):
    s = _searcher(tmp_path, {"m.py": HAN_FORMS})
    cases = [("资料库", "load_settings"), ("读取使用者设定", "load_settings"), ("電子郵件", "notify_admin"),
             ("發送通知給管理員", "notify_admin")]
    assert [(q, [h.name for h in s.search(q, k=1)]) for q, want in cases
            if [h.name for h in s.search(q, k=1)] != [want]] == []


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
