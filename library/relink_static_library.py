# Calls ld to relink a static library into a .o with the exported symbols
# list. This is necessary because the linker doesn't support exporting
# symbols from static libraries. The .o can then be linked into the final
# binary.
# ld -r -x -arch arm64 -o foo.o -exported_symbols_list library/symbols.exports target/aarch64-apple-ios/release/libupdater.a
# becomes:
# relink_static_library.py target/aarch64-apple-ios/release/libupdater.a library/symbols.exports foo.o arm64

import os
import subprocess
import sys
import argparse

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('-i', "--input", help="The path to the static library to relink")
    parser.add_argument("--symbols-file", help="The file containing the exported symbols list")
    parser.add_argument('-o', "--output", help="The path to the .o file to write")
    parser.add_argument('-a', "--arch", default="arm64", help='The architecture to build for, e.g. "arm64"')
    args = parser.parse_args()

    relink_static_library(args.input, args.symbols_file, args.output, args.arch)


def relink_static_library(library_path, symbols_file, output_path, arch):
    subprocess.check_call(
        [
            "ld",
            # -r Merges object files to produce another mach-o object file with file type MH_OBJECT.
            "-r",
            # -x Do not put non-global symbols in the output file's symbol table.
            # Hides non-global symbols from the output file's symbol table and
            # makes dead code stripping work.
            "-x",
            "-o",
            output_path,
            "-exported_symbols_list",
            symbols_file,
            library_path,
            "-arch",
            arch,
        ]
    )
