# Bundled AROS ROM

Copperline boots these AROS m68k ROM images when the user supplies no
Kickstart of their own (see `src/romsearch.rs`). AROS (the AROS Research
Operating System) is an open-source, freely redistributable re-implementation
of the AmigaOS API, licensed under the AROS Public License (`LICENSE`). Unlike
a real Kickstart it can legally ship with the program.

## Files

| File                          | Size      | Maps at  | Role                          |
|-------------------------------|-----------|----------|-------------------------------|
| `aros-amiga-m68k-rom.bin`     | 512 KiB   | $F80000  | Kickstart-replacement ROM     |
| `aros-amiga-m68k-ext.bin`     | 512 KiB   | $E00000  | Extended ROM                  |

The two halves are consumed exactly as WinUAE and FS-UAE take them.

## Provenance

Extracted from the official AROS nightly build, file
`boot/amiga/aros-rom.bin` and `boot/amiga/aros-ext.bin` inside:

    https://sourceforge.net/projects/aros/files/nightly2/20260613/Binaries/AROS-20260613-amiga-m68k-boot-iso.zip

Build date 2026-06-13. The in-repo file names follow the WinUAE/FS-UAE
convention (`aros-amiga-m68k-rom.bin` / `-ext.bin`); the bytes are unchanged.

To refresh: download a newer `amiga-m68k-boot-iso.zip` from the AROS nightly
page, extract the ISO, then pull `boot/amiga/aros-rom.bin` and
`boot/amiga/aros-ext.bin` and rename them here. Both must be exactly 524288
bytes (512 KiB). Also refresh `LICENSE` and `ACKNOWLEDGEMENTS` from the same
archive.
