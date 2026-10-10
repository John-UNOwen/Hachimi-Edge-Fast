#!/usr/bin/env python3
"""C1 check on a linked DLL: what the exported proxy stubs actually are in the shipped file.

`src/windows/proxy/exports.def` is linked into every msvc build (`build.rs`), so all 47 proxy
exports are reachable whatever the deployer renames the cdylib to - including `cri_mana_vpx.dll`,
the name this fork ships under, for which `src/windows/hook.rs` runs no proxy `init` of its own.
C1 is the claim about those stubs: they used to be one instruction, `jmp qword ptr [rip + <cell>]`,
through a `static mut ... : usize = 0`, so a call to any of them executed address 0.

This script reads the linked binary rather than the source, and answers three questions:

  1. Does the export table still carry exactly the names exports.def advertises?
  2. Does every one of them open by reading its cell and testing it
     (`mov rax, [rip + cell]` / `test rax, rax` / `jz <refusal>` / `jmp rax`), with a refusal arm
     that hands the refusal the same cell and answers 0, and does each stub read a cell of its own?
     Or does any of them still open with `jmp qword ptr [rip + cell]`, which is the shape that
     executes 0 while the cell is 0?
  3. With `--call`: map the file the way a game process maps it and call every export. No `DllMain`
     runs (the image is mapped with `DONT_RESOLVE_DLL_REFERENCES`), so no cell has been written -
     the state a call lands on is the empty-cell state. A stub that jumps answers nothing: the host
     takes `0xC0000005` and the script dies with that code. A stub that is inert answers 0 and the
     host survives.

Usage:
    python tools/check_proxy_export_stubs.py [dll ...] [--call]

With no path it checks `target/release/hachimi.dll`, falling back to `target/debug/hachimi.dll`.
Exit 0 only when every advertised export is published, guarded, reading its own cell, and - under
`--call` - answered its call.
"""
from __future__ import annotations

import ctypes
import pathlib
import re
import struct
import sys

DEF = pathlib.Path("src/windows/proxy/exports.def")

# `mov rax, qword ptr [rip + disp32]` / `test rax, rax` / `jmp rax`.
READS_CELL = b"\x48\x8b\x05"
TESTS_CELL = b"\x48\x85\xc0"
JMP_REG = b"\xff\xe0"
# `jmp qword ptr [rip + disp32]`: the C1 shape, an indirect jump with nothing behind the cell.
JUMP_THROUGH_MEMORY = b"\xff\x25"
# The refusal arm `proxy_proc!` writes: `sub rsp, 40` / `lea rcx, [rip + cell]` / `call refusal` /
# `add rsp, 40` / `xor eax, eax` / `ret`. Group 1 is the shadow-space size, group 2 the cell the
# refusal is handed.
REFUSAL_ARM = re.compile(rb"\x48\x83\xec(.)\x48\x8d\x0d(....)\xe8(....)\x48\x83\xc4\1\x31\xc0\xc3")

GUARDED = "reads, tests, branches, then jumps the target"


def advertised_names() -> list[str]:
    names = []
    for line in DEF.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith(";") and line != "EXPORTS":
            names.append(line)
    return names


def parse_pe(data: bytes):
    pe_off = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe_off:pe_off + 4] != b"PE\0\0":
        raise SystemExit("not a PE image")
    coff = pe_off + 4
    machine, n_sections, _t, _sp, _ptr, opt_size, _chars = struct.unpack_from("<HHIIIHH", data, coff)
    opt = coff + 20
    magic, = struct.unpack_from("<H", data, opt)
    if magic != 0x20B:
        raise SystemExit(f"expected PE32+ (a 64 bit cdylib), got {magic:#x}")
    export_dir_rva, export_dir_size = struct.unpack_from("<II", data, opt + 112)
    if export_dir_rva == 0:
        raise SystemExit("the image exports nothing")

    sections = []
    for i in range(n_sections):
        base = opt + opt_size + i * 40
        name = data[base:base + 8].rstrip(b"\0").decode()
        vsize, vaddr, raw_size, raw_off = struct.unpack_from("<IIII", data, base + 8)
        sections.append((name, vaddr, max(vsize, raw_size), raw_off))

    def rva_to_offset(rva: int) -> int:
        for _name, vaddr, span, raw_off in sections:
            if vaddr <= rva < vaddr + span:
                return raw_off + (rva - vaddr)
        raise SystemExit(f"rva {rva:#x} is in no section")

    def rva_to_section(rva: int) -> str:
        for name, vaddr, span, _raw_off in sections:
            if vaddr <= rva < vaddr + span:
                return name
        return "?"

    return machine, export_dir_rva, export_dir_size, rva_to_offset, rva_to_section


def read_exports(data: bytes):
    machine, export_rva, export_size, rva_to_offset, rva_to_section = parse_pe(data)
    header = data[rva_to_offset(export_rva):rva_to_offset(export_rva) + export_size]
    (_flags, _stamp, _major, _minor, _name_rva, ordinal_base, num_funcs, num_names,
     funcs_rva, names_rva, ordinals_rva) = struct.unpack_from("<IIHHIIIIIII", header, 0)

    funcs = data[rva_to_offset(funcs_rva):rva_to_offset(funcs_rva) + num_funcs * 4]
    names = data[rva_to_offset(names_rva):rva_to_offset(names_rva) + num_names * 4]
    ordinals = data[rva_to_offset(ordinals_rva):rva_to_offset(ordinals_rva) + num_names * 2]

    table: dict[str, tuple[int, int]] = {}
    for i in range(num_names):
        nm_rva, = struct.unpack_from("<I", names, i * 4)
        start = rva_to_offset(nm_rva)
        name = data[start:data.index(b"\0", start)].decode()
        ordinal = ordinal_base + struct.unpack_from("<H", ordinals, i * 2)[0]
        func_rva, = struct.unpack_from("<I", funcs, (ordinal - ordinal_base) * 4)
        table[name] = (ordinal, func_rva)

    return machine, num_funcs, num_names, table, rva_to_offset, rva_to_section


def classify_stub(head: bytes, func_rva: int):
    """Return (shape, cell_rva) for the first instructions of one exported stub.

    The guarded shape is the one `proxy_proc!` writes: read the cell, test it, branch to the refusal
    arm when the cell is 0, and only then jump to whatever the cell names. The cold arm is checked
    too, because the arm that refuses a call is the half that keeps a stub inert, and because the
    address it hands the refusal is what names the export in the log.
    """
    if head[:3] != READS_CELL:
        if head[:2] == JUMP_THROUGH_MEMORY:
            disp, = struct.unpack_from("<i", head, 2)
            return "JUMPS THROUGH ITS CELL", func_rva + 6 + disp
        return "neither shape", 0

    disp, = struct.unpack_from("<i", head, 3)
    cell_rva = func_rva + 7 + disp              # rip points at the next instruction

    if head[7:10] != TESTS_CELL:
        return "reads its cell without testing it", cell_rva

    branch = head[10:]
    jz_len = 2 if branch[:1] == b"\x74" else (5 if branch[:2] == b"\x0f\x84" else 0)
    if jz_len == 0:
        return "tests its cell but never branches on it", cell_rva
    if branch[jz_len:jz_len + 2] != JMP_REG:
        return "branches without a guarded jump to the target", cell_rva

    arm = REFUSAL_ARM.search(head)
    if arm is None:
        return "jumps the target but names no refusal arm", cell_rva
    if arm.group(1) != b"\x28":
        return "refusal arm leaves no shadow space", cell_rva

    # `lea rcx, [rip + disp32]` starts four bytes into the arm; the address it computes is relative
    # to the instruction after it, seven bytes on from that `lea`.
    lea_disp, = struct.unpack_from("<i", arm.group(2), 0)
    lea_cell_rva = func_rva + arm.start() + 11 + lea_disp
    if lea_cell_rva != cell_rva:
        return f"refusal arm names cell {lea_cell_rva:#x}, not the cell this stub reads", cell_rva

    return GUARDED, cell_rva


def free(module) -> None:
    # `_handle` is a 64 bit address: it has to be handed back as a c_void_p, not as a Python int.
    ctypes.windll.kernel32.FreeLibrary(ctypes.c_void_p(module._handle))


def check_one(path: pathlib.Path, names: list[str], call: bool) -> int:
    data = path.read_bytes()
    machine, num_funcs, num_names, table, rva_to_offset, rva_to_section = read_exports(data)

    print(f"{path}: machine {machine:#x}, {num_names} exported names, {num_funcs} functions")

    missing = [name for name in names if name not in table]
    # `DllMain` is the mod's own entry point: the linker publishes it whatever the def file says.
    extra = sorted(set(table) - set(names) - {"DllMain"})
    print(f"  exports.def advertises {len(names)}, published and not advertised: {extra}")
    print(f"  advertised and absent from the export table: {len(missing)} {missing}")

    cells: dict[int, list[str]] = {}
    guarded, jumped, odd = 0, 0, 0
    samples = []

    for name in names:
        if name not in table:
            continue
        _ordinal, func_rva = table[name]
        head = data[rva_to_offset(func_rva):rva_to_offset(func_rva) + 48]
        shape, cell_rva = classify_stub(head, func_rva)

        if shape == "JUMPS THROUGH ITS CELL":
            jumped += 1
        elif shape == GUARDED:
            guarded += 1
            cells.setdefault(cell_rva, []).append(name)
        else:
            odd += 1
            print(f"  {name} at rva {func_rva:#x}: {shape}")

        if name in ("UnityMain", "WinHttpOpen", "WinHttpCloseHandle", "WinHttpWebSocketShutdown"):
            samples.append((name, func_rva, shape, cell_rva, head[:38]))

    shared = {cell: owners for cell, owners in cells.items() if len(owners) > 1}
    cell_sections = sorted({rva_to_section(cell) for cell in cells})
    writable = sorted({name for name in cell_sections if name.lstrip(".").startswith(("data", "bss", "tls"))})

    print(f"  stubs that read, test and only then jump: {guarded}")
    print(f"  stubs that still jump through their cell: {jumped}")
    print(f"  stubs in no guarded shape: {odd}")
    print(f"  distinct cells those stubs read: {len(cells)} for {guarded} stubs, shared cells: {len(shared)}")
    for cell, owners in sorted(shared.items()):
        print(f"    cell rva {cell:#x} is read by {owners}")
    print(f"  sections the cells live in: {cell_sections} (writable: {writable})")

    for name, func_rva, shape, cell_rva, head in samples:
        print(f"  {name} at rva {func_rva:#x}: {shape}, cell rva {cell_rva:#x} in {rva_to_section(cell_rva)}")
        print(f"    {head.hex(' ')}")

    static_ok = (not missing and not extra and jumped == 0 and odd == 0 and not shared
                 and len(cells) == guarded and cell_sections and writable == cell_sections)

    if not call:
        return 0 if static_ok else 1

    # Map the image without resolving imports or running `DllMain`: the cells are the ones a fresh
    # attach finds before `hook.rs` has run `proxy::install`, which is the state C1 is about.
    DONT_RESOLVE_DLL_REFERENCES = 0x1
    try:
        module = ctypes.WinDLL(str(path.resolve()), mode=DONT_RESOLVE_DLL_REFERENCES)
    except OSError as exc:
        print(f"  load failed: {exc}")
        return 1

    answered, nonzero = 0, []
    for name in names:
        if name not in table:
            continue
        try:
            result = getattr(module, name)()
        except Exception as exc:  # noqa: BLE001 - a stub that does not answer is the finding
            print(f"  {name} raised {exc!r} instead of answering")
            free(module)
            return 1
        if result == 0:
            answered += 1
        else:
            nonzero.append((name, result))

    free(module)
    print(f"  called with every cell unwritten: {answered}/{len(names)} answered 0, nonzero answers: {nonzero}")

    return 0 if (static_ok and answered == len(names) and not nonzero) else 1


def main() -> int:
    argv = [a for a in sys.argv[1:] if a != "--call"]
    call = "--call" in sys.argv[1:]

    if argv:
        paths = [pathlib.Path(a) for a in argv]
    else:
        release, debug = pathlib.Path("target/release/hachimi.dll"), pathlib.Path("target/debug/hachimi.dll")
        paths = [release if release.exists() else debug]

    names = advertised_names()
    rc = 0
    for path in paths:
        if not path.exists():
            print(f"{path}: not there - build the cdylib first")
            rc = 1
            continue
        rc |= check_one(path, names, call)
    return rc


if __name__ == "__main__":
    sys.exit(main())
