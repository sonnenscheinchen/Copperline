#!/usr/bin/env python3
"""Build a bootable ADF from the assembled boot block and main program.

Layout:
  sector 0-1 (bytes 0..1023)   : boot block (boot.bin), zero-padded, with the
                                 standard Amiga boot-block checksum patched in.
  sector 2+  (bytes 1024..)    : the main program (test.bin); the boot block
                                 loads it to $30000 and jumps to it.

The boot code takes over the machine itself and never touches the filesystem, so
no DOS structures are needed.
"""
import struct
import sys

ADF_SIZE = 901120          # 80 tracks * 2 sides * 11 sectors * 512 bytes
BOOTBLOCK_SIZE = 1024


def boot_checksum(block: bytes) -> int:
    """Standard boot-block checksum: end-around-carry sum of all longwords must
    equal 0xFFFFFFFF, so the checksum field is the one's-complement of the sum of
    every other longword."""
    total = 0
    for i in range(0, BOOTBLOCK_SIZE, 4):
        if i == 4:
            continue                       # skip the checksum field itself
        total += struct.unpack(">I", block[i:i + 4])[0]
        total = (total & 0xFFFFFFFF) + (total >> 32)
    return (~total) & 0xFFFFFFFF


def main() -> int:
    boot_path = sys.argv[1] if len(sys.argv) > 1 else "boot.bin"
    main_path = sys.argv[2] if len(sys.argv) > 2 else "test.bin"
    out = sys.argv[3] if len(sys.argv) > 3 else "timing-test.adf"

    with open(boot_path, "rb") as f:
        boot = f.read()
    with open(main_path, "rb") as f:
        prog = f.read()
    if len(boot) > BOOTBLOCK_SIZE:
        print(f"error: {boot_path} is {len(boot)} bytes, exceeds the boot block")
        return 1
    if BOOTBLOCK_SIZE + len(prog) > ADF_SIZE:
        print(f"error: {main_path} does not fit in the disk")
        return 1

    block = bytearray(boot + b"\x00" * (BOOTBLOCK_SIZE - len(boot)))
    block[4:8] = struct.pack(">I", boot_checksum(block))

    image = bytearray(ADF_SIZE)
    image[:BOOTBLOCK_SIZE] = block
    image[BOOTBLOCK_SIZE:BOOTBLOCK_SIZE + len(prog)] = prog
    with open(out, "wb") as f:
        f.write(image)
    print(f"wrote {out}: boot {len(boot)} B (checksum "
          f"{struct.unpack('>I', block[4:8])[0]:#010x}), main {len(prog)} B")
    return 0


if __name__ == "__main__":
    sys.exit(main())
