"""The Linux function order is the macOS one, under the names another target's build gives."""
import os
import shutil
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "tools"))
from order_file import elf_order, unhashed  # noqa: E402


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
