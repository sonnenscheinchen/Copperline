#!/bin/sh
# Assemble the timing test and wrap it into a bootable ADF.
#
# Needs vasm (the Motorola-syntax m68k assembler, vasmm68k_mot) on PATH or in
# VASM. Get it from http://sun.hasenbraten.de/vasm/ and build with:
#   make CPU=m68k SYNTAX=mot
set -e
cd "$(dirname "$0")"

VASM="${VASM:-vasmm68k_mot}"
if ! command -v "$VASM" >/dev/null 2>&1; then
    echo "error: vasmm68k_mot not found; set VASM=/path/to/vasmm68k_mot" >&2
    exit 1
fi

"$VASM" -Fbin -m68000 -o boot.bin boot.asm
"$VASM" -Fbin -m68000 -o test.bin test.asm
python3 make_adf.py boot.bin test.bin timing-test.adf
echo "built timing-test.adf"
