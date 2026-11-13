"""The Linux function order is the macOS one, under the names another target's build gives."""
import os
import shutil
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "tools"))
from order_file import elf_check, elf_functions, elf_order, unhashed  # noqa: E402

# a stripped rust-lld link of three functions with --symbol-ordering-file listing gamma, alpha
MAP = """\
             VMA              LMA     Size Align Out     In      Symbol
          2001a8           2001a8       54     1         m.o:(.eh_frame+0x2c)
          201200           201200       4b    16 .text
          201200           201200        6     8         m.o:(.text._RNvCs8AlEeC1jcYO_1m5gamma)
          201200           201200        6     1                 _RNvCs8AlEeC1jcYO_1m5gamma
          201208           201208        6     8         m.o:(.text._RNvCs8AlEeC1jcYO_1m5alpha)
          201208           201208        6     1                 _RNvCs8AlEeC1jcYO_1m5alpha
          201210           201210        6     8         m.o:(.text._RNvCs8AlEeC1jcYO_1m4beta)
          201210           201210        6     1                 _RNvCs8AlEeC1jcYO_1m4beta
          201220           201220       2b    16         m.o:(.text._start)
          201220           201220       2b     1                 _start
          202250           202250       18     8 .got
          202250           202250       18     8         <internal>:(.got)
"""


def test_the_link_map_gives_the_functions_in_text_and_the_check_reads_their_order(tmp_path):
    (tmp_path / "m.map").write_text(MAP)
    have = elf_functions(str(tmp_path / "m.map"))
    assert have == {
        "_RNvCs8AlEeC1jcYO_1m5gamma": 0x201200,
        "_RNvCs8AlEeC1jcYO_1m5alpha": 0x201208,
        "_RNvCs8AlEeC1jcYO_1m4beta": 0x201210,
        "_start": 0x201220,
    }
    (tmp_path / "in_order").write_text("_RNvCs8AlEeC1jcYO_1m5gamma\nmissing\n_RNvCs8AlEeC1jcYO_1m5alpha\n")
    (tmp_path / "out_of_order").write_text("_start\n_RNvCs8AlEeC1jcYO_1m5alpha\n")
    assert elf_check(str(tmp_path / "m.map"), str(tmp_path / "in_order"), first=2) == 0
    assert elf_check(str(tmp_path / "m.map"), str(tmp_path / "out_of_order"), first=2) == 1
    assert elf_check(str(tmp_path / "m.map"), str(tmp_path / "in_order"), first=3) == 1


def test_names_match_across_crate_hashes_in_the_listed_order():
    gnu = {
        "main": "main",
        "_RNvCsAAA_8scrutate4main": "scrutate[0a1b]::main",
        "_RNvCsBBB_3std2rt10lang_start": "std[2c3d]::rt::lang_start",
        "_RNvCsBBB_3std2rt10lang_start.cold": "std[2c3d]::rt::lang_start.cold",
        "_RNvCsCCC_7unknown3fun": "unknown[4e5f]::fun",
    }
    macho = {
        "main": "main",
        "_RNvCsXXX_8scrutate4main": "scrutate[9a9a]::main",
        "_RNvCsYYY_3std2rt10lang_start": "std[8b8b]::rt::lang_start",
        "gone": "gone",
    }
    def demangle(names):
        return [gnu.get(n) or macho[n] for n in names]

    listed = ["__RNvCsYYY_3std2rt10lang_start", "_main", "__RNvCsXXX_8scrutate4main", "_gone"]
    assert elf_order(listed, list(gnu), demangle) == ["_RNvCsBBB_3std2rt10lang_start", "main", "_RNvCsAAA_8scrutate4main"]


def test_hashes_go_and_generic_arguments_stay():
    assert unhashed("<alloc[1f]::vec::Vec<u8>>::push") == "<alloc::vec::Vec<u8>>::push"
    assert unhashed("core::ptr::drop_glue::<[u8; 4]>") == "core::ptr::drop_glue::<[u8; 4]>"
    assert unhashed("<core[9e]::slice::<[f32]>>::len") == "<core::slice::<[f32]>>::len"


@pytest.mark.skipif(shutil.which("c++filt") is None, reason="needs c++filt")
def test_the_system_demangler_matches_two_builds_of_one_function():
    # the same function from the aarch64 and the x86_64 macOS release builds
    listed = ["__RNvCs6zAwaxlsK9K_8scrutate4main"]
    assert elf_order(listed, ["_RNvCseQL6ipDOQZa_8scrutate4main", "_RNvCseQL6ipDOQZa_8scrutate5other"]) == ["_RNvCseQL6ipDOQZa_8scrutate4main"]
