#!/usr/bin/env python3
r"""Generate src/il2cpp/slot_table_generated.rs from recon evidence.

The Global Android build of the game ships a hollowed libil2cpp.so (no il2cpp_*
exports; runtime dynamic section zeroed), so dlsym cannot resolve the il2cpp C API.
libunity.so contains a generated Il2CppApi compat layer instead: an init routine
resolves every il2cpp_* name through an internal resolver and stores the resulting
function pointers in a flat table inside libunity's BSS.

This script turns the extracted (name -> slot) mapping into a versioned Rust table
plus the build fingerprint used to validate the layout at runtime.

PROVENANCE -- read this before running the script.
    src/il2cpp/slot_table.rs reads the constants written here straight out of the
    running game, so every value in the generated file must be a fact about the
    inputs of the run that produced it. This script therefore carries no layout
    constants and no default input paths: a literal in this file is one game build's
    measurement, and a table that keeps it after an update pins the runtime to a
    layout nobody re-measured. Offsets come from the supplied inputs, or the run
    stops.

    Where each emitted value comes from:
      TABLE_OFFSET / API_CTX_OFFSET / INIT_FN_OFFSET / RESOLVER_OFFSET
          the provenance header of --table, written by the extraction step that
          disassembled --libunity:
              # table_base_offset <vaddr>    # api_ctx_global <vaddr>
              # init_fn           <vaddr>    # resolver       <vaddr>
          A missing key is an error, not a fallback to a built-in value.
      the vaddr -> file-offset mapping
          the PT_LOAD program headers of --libunity, per segment (no assumed code
          bias: this file really maps its rodata at bias 0 and its code at 0x4000).
      INIT_FN_FINGERPRINT / RESOLVER_FINGERPRINT
          FINGERPRINT_LEN bytes of --libunity at the init_fn / resolver vaddrs, so a
          stale vaddr reads different bytes instead of passing silently.
      SLOTS / SLOT_COUNT
          the data rows of --table. Every Slot.name_off is checked to hold
          "<name>\0" at that vaddr in --libunity -- the same check validate() makes
          at runtime for the sampled slots, applied to every row before the file is
          written.
      LIBUNITY_FILE_SIZE
          the size of --libunity.

    Cross-checks between the two inputs (a disagreement is not papered over):
      * the header's `slots` count matches the parsed rows and `entry_size` is 8;
      * the table span table_base_offset + slots*entry_size lies inside one PT_LOAD
        of --libunity, and that segment is writable;
      * api_ctx_global sits directly after the table, where the 1.35.1 disassembly
        puts it (warns if a newer build moved it);
      * every resolver_call_va row of --table is an AArch64 BL to the header's
        `resolver`;
      * VALIDATION_SLOTS below stay inside the supplied table.

Inputs (all required; none of them is in this repository -- they are the recon
workspace of the build named by --game-version):
  --table         name_slot_table.txt   provenance header, then one row per slot:
                                        index \t name \t name_string_off \t resolver_call_va
  --libunity      libunity.so           the ELF those offsets and names were read from
  --game-version  the game build the table belongs to
Output:
  src/il2cpp/slot_table_generated.rs (--out to write elsewhere)

  # rebuild the table for a new build, then prove the committed file is what the
  # inputs produce (--check writes nothing and exits non-zero on drift):
  python tools/gen_il2cpp_slot_table.py --table "$RECON/name_slot_table.txt" \
      --libunity "$RECON/libunity.so" --game-version 1.35.1
  python tools/gen_il2cpp_slot_table.py --check --table "$RECON/name_slot_table.txt" \
      --libunity "$RECON/libunity.so" --game-version 1.35.1
"""
import argparse
import os
import re
import struct
import sys

DEFAULT_OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                           "..", "src", "il2cpp", "slot_table_generated.rs")

FINGERPRINT_LEN = 16
# Slots the runtime samples to prove the layout before the table is trusted. A
# sampling plan rather than a layout fact, but it has to stay inside the supplied
# table, so it is checked against the row count instead of being assumed.
VALIDATION_SLOTS = [0, 1, 2, 40, 80, 120, 160, 200, 233]

# Provenance header keys --table must carry: header key -> constant it feeds.
NEEDED_KEYS = {
    "table_base_offset": "TABLE_OFFSET",
    "api_ctx_global": "API_CTX_OFFSET",
    "init_fn": "INIT_FN_OFFSET",
    "resolver": "RESOLVER_OFFSET",
}

ENTRY_SIZE = 8  # one pointer-sized entry per slot, matching Slot.slot's use below
PT_LOAD = 1
PF_X = 1
PF_W = 2
EM_AARCH64 = 183
# Elf64_Ehdr / Elf64_Phdr, little-endian, no padding.
EHDR_FMT = "<16sHHIQQQIHHHHHH"
PHDR_FMT = "<IIQQQQQQ"
PHDR_SIZE = 56
# bits 31-26 of an AArch64 BL.
BL_OPCODE = 0b100101
HEADER_PAIR = re.compile(r"([A-Za-z_][A-Za-z0-9_]*)\s+((?:0x)?[0-9A-Fa-f]+)\b")


def die(message):
    raise SystemExit("error: " + message)


def warn(message):
    print("warning: " + message, file=sys.stderr)


def readable_file(text):
    if not os.path.isfile(text):
        raise argparse.ArgumentTypeError(
            "%r is not a file. Every input must be passed explicitly; this script "
            "has no default input path." % text)
    return text


def read_table(path):
    """(provenance header, data rows) from name_slot_table.txt."""
    header = {}
    rows = []
    for lineno, line in enumerate(open(path, encoding="utf-8"), 1):
        if line.startswith("#"):
            for key, value in HEADER_PAIR.findall(line):
                try:
                    header.setdefault(key, int(value, 0))
                except ValueError:
                    pass  # prose that looks like a key/value pair, not a provenance line
            continue
        if not line.strip():
            continue
        parts = line.rstrip("\n").split("\t")
        if parts[1:2] == ["<MISSING>"]:
            continue
        if len(parts) < 3:
            die("%s line %d is not 'index\\tname\\tname_string_off[\\tresolver_call_va]': %r"
                % (path, lineno, line.rstrip("\n")))
        try:
            index = int(parts[0])
            name_off = int(parts[2], 16)
        except ValueError:
            die("%s line %d has a non-numeric index or name_string_off: %r"
                % (path, lineno, line.rstrip("\n")))
        call = None
        if len(parts) > 3:
            try:
                call = int(parts[3], 16)
            except ValueError:
                call = None
        rows.append((index, parts[1], name_off, call))
    rows.sort(key=lambda r: r[0])
    return header, rows


def elf_segments(data, path):
    """PT_LOAD segments of an ELF64 object, as (flags, offset, vaddr, filesz, memsz)."""
    if len(data) < 64 or data[:4] != b"\x7fELF":
        die("%s is not an ELF object" % path)
    if data[4] != 2 or data[5] != 1:
        die("%s is not a little-endian ELF64 object" % path)
    ehdr = struct.unpack_from(EHDR_FMT, data, 0)
    machine, phoff, phentsize, phnum = ehdr[2], ehdr[5], ehdr[9], ehdr[10]
    if machine != EM_AARCH64:
        die("%s is machine %d, not AArch64; this table describes the Android arm64 libunity"
            % (path, machine))
    if phentsize < PHDR_SIZE or phoff + phnum * phentsize > len(data):
        die("%s has an unreadable program header table (phoff %#x phentsize %d phnum %d)"
            % (path, phoff, phentsize, phnum))
    loads = []
    for i in range(phnum):
        p_type, p_flags, p_offset, p_vaddr, _, p_filesz, p_memsz, _ = struct.unpack_from(
            PHDR_FMT, data, phoff + i * phentsize)
        if p_type == PT_LOAD:
            loads.append((p_flags, p_offset, p_vaddr, p_filesz, p_memsz))
    if not loads:
        die("%s has no PT_LOAD segment" % path)
    return loads


def containing(loads, start, end, by_memsz=False):
    """The segment whose extent covers [start, end), or None."""
    for seg in loads:
        flags, offset, vaddr, filesz, memsz = seg
        if vaddr <= start and end <= vaddr + (memsz if by_memsz else filesz):
            return seg
    return None


def read_at(data, loads, path, vaddr, length, what):
    """`length` bytes of `path` at the vaddr, mapped through this file's own headers."""
    seg = containing(loads, vaddr, vaddr + length)
    if seg is None:
        die("%s is not inside a file-backed PT_LOAD of %s (vaddr %#x)" % (what, path, vaddr))
    pos = seg[1] + (vaddr - seg[2])
    chunk = data[pos:pos + length]
    if len(chunk) != length:
        die("%s runs past the end of %s (vaddr %#x)" % (what, path, vaddr))
    return chunk


def check_slots(header, rows, path):
    if not rows:
        die("no data rows parsed from %s" % path)
    indices = [r[0] for r in rows]
    if indices != list(range(len(rows))):
        die("slot indices in %s are not contiguous 0..N-1: %r" % (path, indices[:10]))
    if header.get("slots") not in (None, len(rows)):
        die("%s declares %d slots in its provenance header but has %d data rows"
            % (path, header["slots"], len(rows)))
    if header.get("entry_size") not in (None, ENTRY_SIZE):
        die("%s declares entry_size %d; the generated table is one %d-byte entry per slot"
            % (path, header["entry_size"], ENTRY_SIZE))
    names = [r[1] for r in rows]
    if len(set(names)) != len(names):
        die("duplicate names in %s" % path)
    if max(VALIDATION_SLOTS) >= len(rows):
        die("VALIDATION_SLOTS samples slot %d, which %s does not supply (%d slots)"
            % (max(VALIDATION_SLOTS), path, len(rows)))


def check_fingerprints(data, loads, path, init_off, resolver_off):
    """The fingerprint bytes at the two code vaddrs, read out of the supplied file."""
    init_fp = read_at(data, loads, path, init_off, FINGERPRINT_LEN, "the init routine")
    res_fp = read_at(data, loads, path, resolver_off, FINGERPRINT_LEN, "the resolver")
    for vaddr, name in ((init_off, "init routine"), (resolver_off, "resolver")):
        seg = containing(loads, vaddr, vaddr + FINGERPRINT_LEN)
        if seg is not None and not seg[0] & PF_X:
            warn("the %s at %#x is not in an executable PT_LOAD of %s" % (name, vaddr, path))
    return init_fp, res_fp


def check_table_span(loads, table_path, lib_path, table_off, slots):
    end = table_off + slots * ENTRY_SIZE
    seg = containing(loads, table_off, end, by_memsz=True)
    if seg is None:
        die("the function-pointer table %#x..%#x (from %s) is not inside one PT_LOAD of %s; "
            "the supplied offsets do not describe this libunity" % (table_off, end, table_path, lib_path))
    if not seg[0] & PF_W:
        warn("the table %#x..%#x sits in a segment with flags %#x, not a writable one in %s"
             % (table_off, end, seg[0], lib_path))
    return end


def check_name_strings(data, loads, table_path, lib_path, rows):
    """Every recorded name_string_off must really hold that name in libunity."""
    bad = []
    for index, name, name_off, _ in rows:
        raw = read_at(data, loads, lib_path, name_off, len(name) + 1,
                      "slot %d name string" % index)
        if raw != name.encode("utf-8") + b"\0":
            bad.append((index, name, name_off, raw[:40]))
    if bad:
        index, name, name_off, seen = bad[0]
        die("%s says slot %d (%s) has its name at %#x, but %s holds %r there "
            "(%d of %d rows disagree)"
            % (table_path, index, name, name_off, lib_path, seen, len(bad), len(rows)))


def check_resolver_calls(data, loads, table_path, lib_path, resolver_off, rows):
    """The resolver_call_va column must branch to the resolver the header names."""
    decoded = 0
    for index, name, _, site in rows:
        if site is None:
            continue
        raw = read_at(data, loads, lib_path, site, 4, "slot %d resolver call" % index)
        word = struct.unpack("<I", raw)[0]
        if word >> 26 != BL_OPCODE:
            die("the resolver_call_va %#x of slot %d (%s) in %s is not a BL (word %#x in %s)"
                % (site, index, name, table_path, word, lib_path))
        imm = word & 0x3FF_FFFF
        if imm & (1 << 25):
            imm -= 1 << 26
        target = site + imm * 4
        decoded += 1
        if target != resolver_off:
            die("the resolver_call_va %#x of slot %d (%s) branches to %#x, but %s declares "
                "resolver %#x" % (site, index, name, target, table_path, resolver_off))
    if not decoded:
        warn("%s carries no usable resolver_call_va column, so %s was not cross-checked "
             "against the supplied call sites" % (table_path, "the resolver offset"))


def render(args, rows, data, init_fp, res_fp, table_off, ctx_off, init_off, resolver_off):
    out = []
    out.append("// @generated by tools/gen_il2cpp_slot_table.py -- do not edit by hand.")
    out.append("//")
    out.append("// Global Android il2cpp C-API slot table, extracted from libunity.so.")
    out.append("// Game build: %s   libunity.so size: %d bytes" % (args.game_version, len(data)))
    out.append("//")
    out.append("// The Global build hollows libil2cpp.so (no il2cpp_* exports, runtime dynsym")
    out.append("// zeroed) so dlsym cannot find the API. libunity.so instead carries a generated")
    out.append("// Il2CppApi compat layer: the routine at INIT_FN_OFFSET resolves every name via")
    out.append("// RESOLVER_OFFSET and stores the pointers at TABLE_OFFSET + slot*8. The")
    out.append("// fingerprints below pin that exact layout; add/carry a new table per game build.")
    out.append("")
    out.append("/// Base of the function-pointer table, relative to the libunity load address.")
    out.append("pub const TABLE_OFFSET: usize = 0x%x;" % table_off)
    out.append("/// Number of entries in the table.")
    out.append("pub const SLOT_COUNT: usize = %d;" % len(rows))
    out.append("/// API context global, set by the init routine just before it fills the table.")
    out.append("pub const API_CTX_OFFSET: usize = 0x%x;" % ctx_off)
    out.append("/// Init routine that fills the table.")
    out.append("pub const INIT_FN_OFFSET: usize = 0x%x;" % init_off)
    out.append("/// Internal name -> pointer resolver.")
    out.append("pub const RESOLVER_OFFSET: usize = 0x%x;" % resolver_off)
    out.append("/// On-disk libunity.so size, for documentation/telemetry only.")
    out.append("pub const LIBUNITY_FILE_SIZE: u64 = %d;" % len(data))
    out.append("")
    out.append("/// First %d bytes of the init routine (layout fingerprint)." % FINGERPRINT_LEN)
    out.append("pub const INIT_FN_FINGERPRINT: [u8; %d] = [%s];"
               % (FINGERPRINT_LEN, ", ".join("0x%02x" % b for b in init_fp)))
    out.append("/// First %d bytes of the resolver (layout fingerprint)." % FINGERPRINT_LEN)
    out.append("pub const RESOLVER_FINGERPRINT: [u8; %d] = [%s];"
               % (FINGERPRINT_LEN, ", ".join("0x%02x" % b for b in res_fp)))
    out.append("")
    out.append("/// Slots sampled at runtime to prove the layout before the table is trusted.")
    out.append("pub const VALIDATION_SLOTS: [usize; %d] = [%s];"
               % (len(VALIDATION_SLOTS), ", ".join(str(s) for s in VALIDATION_SLOTS)))
    out.append("")
    out.append("/// One il2cpp C-API entry.")
    out.append("pub struct Slot {")
    out.append("    /// Function name as requested by the mod.")
    out.append("    pub name: &'static str,")
    out.append("    /// ELF vaddr of the bare name string in libunity rodata (layout check).")
    out.append("    pub name_off: usize,")
    out.append("    /// Index into the function-pointer table.")
    out.append("    pub slot: u16,")
    out.append("}")
    out.append("")
    out.append("/// Table contents in slot order.")
    out.append("pub static SLOTS: &[Slot] = &[")
    for index, name, name_off, _ in rows:
        out.append('    Slot { name: "%s", name_off: 0x%x, slot: %d },'
                   % (name, name_off, index))
    out.append("];")
    out.append("")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(
        description="Generate src/il2cpp/slot_table_generated.rs from recon evidence.")
    ap.add_argument("--table", required=True, type=readable_file,
                    help="name_slot_table.txt: provenance header, then index, name, "
                         "name_string_off, resolver_call_va")
    ap.add_argument("--libunity", required=True, type=readable_file,
                    help="the libunity.so those offsets were read from")
    ap.add_argument("--game-version", required=True,
                    help="game build the supplied offsets belong to")
    ap.add_argument("--out", default=DEFAULT_OUT,
                    help="generated module to write (default: src/il2cpp/slot_table_generated.rs)")
    ap.add_argument("--check", action="store_true",
                    help="compare --out with what the supplied inputs produce; write nothing, "
                         "exit non-zero on drift")
    args = ap.parse_args()

    header, rows = read_table(args.table)
    for key, const in sorted(NEEDED_KEYS.items()):
        if key not in header:
            die("%s has no '# %s <vaddr>' provenance line, so %s cannot come from the "
                "supplied inputs; this script keeps no built-in offsets" % (args.table, key, const))
    table_off = header["table_base_offset"]
    ctx_off = header["api_ctx_global"]
    init_off = header["init_fn"]
    resolver_off = header["resolver"]

    check_slots(header, rows, args.table)

    data = open(args.libunity, "rb").read()
    loads = elf_segments(data, args.libunity)
    init_fp, res_fp = check_fingerprints(data, loads, args.libunity, init_off, resolver_off)
    table_end = check_table_span(loads, args.table, args.libunity, table_off, len(rows))
    if ctx_off != table_end:
        warn("%s declares api_ctx_global %#x, not %#x (the table's %d entries end there); "
             "check the disassembly of %s" % (args.table, ctx_off, table_end, len(rows), args.libunity))
    check_name_strings(data, loads, args.table, args.libunity, rows)
    check_resolver_calls(data, loads, args.table, args.libunity, resolver_off, rows)

    text = render(args, rows, data, init_fp, res_fp, table_off, ctx_off, init_off, resolver_off)

    print("%s: %d bytes, %d PT_LOAD segment(s); vaddr -> file offset taken from its "
          "program headers" % (args.libunity, len(data), len(loads)))
    print("%s: %d slots; table %#x..%#x, api_ctx %#x, init %#x, resolver %#x taken from "
          "its provenance header" % (args.table, len(rows), table_off, table_end,
                                     ctx_off, init_off, resolver_off))

    if args.check:
        if not os.path.isfile(args.out):
            die("--check: %s does not exist" % args.out)
        # Universal newlines: git checks this file out with CRLF on Windows
        # (core.autocrlf), which is the same content the generator writes.
        current = open(args.out, encoding="utf-8").read()
        if current == text:
            print("check: %s is exactly what the supplied inputs produce" % args.out)
            return
        lines, want = current.split("\n"), text.split("\n")
        at = next((i for i, (a, b) in enumerate(zip(lines, want)) if a != b),
                  min(len(lines), len(want)))
        die("%s does not match the supplied inputs at line %d:\n  file: %s\n  inputs: %s"
            % (args.out, at + 1, lines[at] if at < len(lines) else "<end of file>",
               want[at] if at < len(want) else "<end of generated text>"))

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print("wrote %s (%d slots)" % (os.path.abspath(args.out), len(rows)))


if __name__ == "__main__":
    main()
