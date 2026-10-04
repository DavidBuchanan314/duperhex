"""Minimal x86-64 emulator for calling functions in the game binary (unicorn + pyelftools).

Loads the executable's segments, routes imported functions to Python stubs, lets individual internal
functions be replaced by Python stubs, and models libstdc++'s old-ABI (copy-on-write) std::string.
"""
import struct

from elftools.elf.elffile import ELFFile
from elftools.elf.relocation import RelocationSection
from unicorn import Uc, UcError, UC_ARCH_X86, UC_MODE_64, UC_HOOK_CODE, UC_PROT_ALL
from unicorn.x86_const import (
    UC_X86_REG_RAX, UC_X86_REG_RDI, UC_X86_REG_RSI, UC_X86_REG_RDX, UC_X86_REG_RCX,
    UC_X86_REG_R8, UC_X86_REG_R9, UC_X86_REG_RSP, UC_X86_REG_RIP, UC_X86_REG_XMM0,
    UC_X86_REG_XMM1, UC_X86_REG_FS_BASE,
)

PAGE = 0x1000
TRAP_BASE = 0x7000_0000   # one 16-byte slot per imported function; RET instructions
TRAP_SIZE = 0x10_0000
SENTINEL = TRAP_BASE + TRAP_SIZE - 0x10
HEAP_BASE = 0x5000_0000
HEAP_SIZE = 0x1000_0000
STACK_BASE = 0x7f00_0000
STACK_SIZE = 0x10_0000
TLS_BASE = 0x7e00_0000
ARG_REGS = [UC_X86_REG_RDI, UC_X86_REG_RSI, UC_X86_REG_RDX, UC_X86_REG_RCX, UC_X86_REG_R8, UC_X86_REG_R9]


def _align_down(x):
    return x & ~(PAGE - 1)


def _align_up(x):
    return (x + PAGE - 1) & ~(PAGE - 1)


class EmuError(Exception):
    pass


class Emu:
    def __init__(self, path):
        with open(path, 'rb') as f:
            self.image = f.read()
        with open(path, 'rb') as f:
            elf = ELFFile(f)
            self.uc = Uc(UC_ARCH_X86, UC_MODE_64)
            self._load_segments(elf)
            self.symbols = {}
            self.sizes = {}
            for sec in elf.iter_sections():
                if sec.name in ('.symtab', '.dynsym'):
                    for sym in sec.iter_symbols():
                        if sym.name and sym['st_value'] and sym.name not in self.symbols:
                            self.symbols[sym.name] = sym['st_value']
                            self.sizes[sym.name] = sym['st_size']
            imports = []
            for sec in elf.iter_sections():
                if isinstance(sec, RelocationSection):
                    symtab = elf.get_section(sec['sh_link'])
                    for rel in sec.iter_relocations():
                        rtype = rel['r_info_type']
                        if rtype in (6, 7):  # R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT
                            name = symtab.get_symbol(rel['r_info_sym']).name
                            imports.append((name, rel['r_offset']))
        self.uc.mem_map(TRAP_BASE, TRAP_SIZE, UC_PROT_ALL)
        self.uc.mem_write(TRAP_BASE, b'\xc3' * TRAP_SIZE)
        self.trap_names = {}
        for i, (name, got) in enumerate(imports):
            addr = TRAP_BASE + 0x10 * i
            self.trap_names[addr] = name
            self.uc.mem_write(got, struct.pack('<Q', addr))
        self.uc.hook_add(UC_HOOK_CODE, self._on_trap, begin=TRAP_BASE, end=TRAP_BASE + TRAP_SIZE - 1)
        self.uc.mem_map(HEAP_BASE, HEAP_SIZE, UC_PROT_ALL)
        self.heap_next = HEAP_BASE
        self.uc.mem_map(STACK_BASE, STACK_SIZE, UC_PROT_ALL)
        self.uc.mem_map(TLS_BASE, PAGE * 4, UC_PROT_ALL)
        self.uc.reg_write(UC_X86_REG_FS_BASE, TLS_BASE + PAGE * 2)
        self.import_stubs = {}
        self.func_stubs = {}
        self.lenient = False
        self.stub_saved = {}
        self.empty_rep = self.sym('_ZNSs4_Rep20_S_empty_rep_storageE')
        self._install_libc()

    def _load_segments(self, elf):
        for seg in elf.iter_segments():
            if seg['p_type'] != 'PT_LOAD':
                continue
            start = _align_down(seg['p_vaddr'])
            end = _align_up(seg['p_vaddr'] + seg['p_memsz'])
            self.uc.mem_map(start, end - start, UC_PROT_ALL)
            self.uc.mem_write(seg['p_vaddr'], seg.data())

    # --- symbols, memory, registers -------------------------------------------------------------

    def sym(self, name):
        if name not in self.symbols:
            raise EmuError('symbol not found: ' + name)
        return self.symbols[name]

    def read(self, addr, n):
        return bytes(self.uc.mem_read(addr, n))

    def write(self, addr, data):
        self.uc.mem_write(addr, bytes(data))

    def u64(self, addr):
        return struct.unpack('<Q', self.read(addr, 8))[0]

    def i32(self, addr):
        return struct.unpack('<i', self.read(addr, 4))[0]

    def f32(self, addr):
        return struct.unpack('<f', self.read(addr, 4))[0]

    def put_u64(self, addr, v):
        self.write(addr, struct.pack('<Q', v & 0xffffffffffffffff))

    def put_i32(self, addr, v):
        self.write(addr, struct.pack('<i', v))

    def put_f32(self, addr, v):
        self.write(addr, struct.pack('<f', v))

    def cstr(self, addr):
        out = bytearray()
        while True:
            chunk = self.read(addr + len(out), 64)
            i = chunk.find(b'\0')
            if i >= 0:
                return bytes(out + chunk[:i])
            out += chunk

    def reg(self, r):
        return self.uc.reg_read(r)

    def arg(self, i):
        return self.uc.reg_read(ARG_REGS[i])

    def stack_arg(self, i):
        """The i-th argument passed on the stack (32-bit), read on entry to a stubbed function."""
        v = self.u64(self.uc.reg_read(UC_X86_REG_RSP) + 8 * (i + 1)) & 0xffffffff
        return v - (1 << 32) if v & 0x80000000 else v

    def iarg(self, i):
        v = self.arg(i) & 0xffffffff
        return v - (1 << 32) if v & 0x80000000 else v

    def farg(self, i=0):
        r = [UC_X86_REG_XMM0, UC_X86_REG_XMM1][i]
        return struct.unpack('<f', struct.pack('<I', self.uc.reg_read(r) & 0xffffffff))[0]

    def ret(self, v):
        self.uc.reg_write(UC_X86_REG_RAX, v & 0xffffffffffffffff)

    def ret_float(self, v):
        self.uc.reg_write(UC_X86_REG_XMM0, struct.unpack('<I', struct.pack('<f', v))[0])

    def alloc(self, n, zero=True):
        n = (n + 15) & ~15
        addr = self.heap_next
        self.heap_next += n
        if self.heap_next > HEAP_BASE + HEAP_SIZE:
            raise EmuError('emulated heap exhausted')
        if zero:
            self.write(addr, b'\0' * n)
        return addr

    def reset_heap(self, mark):
        self.heap_next = mark

    # --- stubs and calls ------------------------------------------------------------------------

    def stub_import(self, name, fn):
        self.import_stubs[name] = fn

    def stub(self, name, fn):
        """Replace an internal function (by symbol name) with a Python function."""
        addr = self.sym(name)
        if addr not in self.func_stubs:
            if addr not in self.stub_saved:
                self.stub_saved[addr] = self.read(addr, 1)
                self.uc.hook_add(UC_HOOK_CODE, self._on_func, begin=addr, end=addr)
            self.write(addr, b'\xc3')  # RET runs after the hook
            self.uc.ctl_remove_cache(addr, addr + 1)  # code already translated must see the change
        self.func_stubs[addr] = fn

    def unstub(self, name):
        """Let a stubbed internal function run its own code again."""
        addr = self.sym(name)
        if addr in self.func_stubs:
            del self.func_stubs[addr]
            self.write(addr, self.stub_saved[addr])
            self.uc.ctl_remove_cache(addr, addr + 1)

    def _on_trap(self, uc, addr, size, _):
        if addr == SENTINEL:
            uc.emu_stop()
            return
        name = self.trap_names.get(addr)
        fn = self.import_stubs.get(name)
        if fn is None and self.lenient:
            self.ret(0)  # unavailable here (platform, Steam, GL): nothing, and 0
            return
        if fn is None:
            self._error = EmuError('unhandled import: %s (called from %#x)' % (name, self.u64(self.reg(UC_X86_REG_RSP))))
            uc.emu_stop()
            return
        try:
            fn(self)
        except Exception as e:  # surface Python errors from inside the emulator callback
            self._error = e
            uc.emu_stop()

    def _on_func(self, uc, addr, size, _):
        if addr not in self.func_stubs:
            return
        try:
            self.func_stubs[addr](self)
        except Exception as e:
            self._error = e
            uc.emu_stop()

    def call(self, fn, *args, floats=()):
        """Call a function by symbol name or address with integer args (and float args in XMM0..)."""
        addr = self.sym(fn) if isinstance(fn, str) else fn
        for r, v in zip(ARG_REGS, args):
            self.uc.reg_write(r, v & 0xffffffffffffffff)
        for r, v in zip([UC_X86_REG_XMM0, UC_X86_REG_XMM1], floats):
            self.uc.reg_write(r, struct.unpack('<I', struct.pack('<f', v))[0])
        rsp = STACK_BASE + STACK_SIZE - 0x1000
        self.put_u64(rsp, SENTINEL)
        self.uc.reg_write(UC_X86_REG_RSP, rsp)
        self._error = None
        try:
            self.uc.emu_start(addr, SENTINEL)
        except UcError as e:
            raise EmuError('%s at %#x' % (e, self.reg(UC_X86_REG_RIP)))
        if self._error:
            raise self._error
        return self.reg(UC_X86_REG_RAX)

    # --- std::string (libstdc++ copy-on-write ABI) ----------------------------------------------
    # A string object holds a pointer to its characters; a header {length, capacity, refcount}
    # sits in the 24 bytes before them. The shared empty string's header is in the binary.

    def new_rep(self, data, capacity=None):
        data = bytes(data)
        cap = max(len(data), capacity or 0)
        rep = self.alloc(24 + cap + 1)
        self.write(rep, struct.pack('<QQi', len(data), cap, 0))
        self.write(rep + 24, data + b'\0')
        return rep + 24

    def string_get(self, obj):
        p = self.u64(obj)
        n = self.u64(p - 24)
        return self.read(p, n)

    def string_set(self, obj, data):
        self.put_u64(obj, self.new_rep(data))

    def string_init(self, obj):
        self.put_u64(obj, self.empty_rep + 24)

    def new_string(self, data=b''):
        obj = self.alloc(8)
        self.string_set(obj, data)
        return obj

    def _install_libc(self):
        s = self.stub_import

        def str_from_cstr(e):  # string(char const*, allocator const&)
            e.string_set(e.arg(0), e.cstr(e.arg(1)))

        def str_copy(e):  # string(string const&)
            e.string_set(e.arg(0), e.string_get(e.arg(1)))

        def str_substr_ctor(e):  # string(string const&, pos, n)
            d = e.string_get(e.arg(1))
            pos, n = e.arg(2), e.arg(3)
            e.string_set(e.arg(0), d[pos:pos + n])

        def assign_cstr(e):
            e.string_set(e.arg(0), e.cstr(e.arg(1)))
            e.ret(e.arg(0))

        def assign_cstr_n(e):
            e.string_set(e.arg(0), e.read(e.arg(1), e.arg(2)))
            e.ret(e.arg(0))

        def assign_str(e):
            e.string_set(e.arg(0), e.string_get(e.arg(1)))
            e.ret(e.arg(0))

        def append_cstr(e):
            e.string_set(e.arg(0), e.string_get(e.arg(0)) + e.cstr(e.arg(1)))
            e.ret(e.arg(0))

        def append_cstr_n(e):
            e.string_set(e.arg(0), e.string_get(e.arg(0)) + e.read(e.arg(1), e.arg(2)))
            e.ret(e.arg(0))

        def append_str(e):
            e.string_set(e.arg(0), e.string_get(e.arg(0)) + e.string_get(e.arg(1)))
            e.ret(e.arg(0))

        def insert_cstr_n(e):  # insert(pos, char const*, n)
            d = e.string_get(e.arg(0))
            pos = e.arg(1)
            e.string_set(e.arg(0), d[:pos] + e.read(e.arg(2), e.arg(3)) + d[pos:])
            e.ret(e.arg(0))

        def push_back(e):
            e.string_set(e.arg(0), e.string_get(e.arg(0)) + bytes([e.arg(1) & 0xff]))

        def swap(e):
            a, b = e.u64(e.arg(0)), e.u64(e.arg(1))
            e.put_u64(e.arg(0), b)
            e.put_u64(e.arg(1), a)

        def reserve(e):  # keep contents, guarantee capacity
            d = e.string_get(e.arg(0))
            e.put_u64(e.arg(0), e.new_rep(d, e.arg(1)))

        def noop(e):
            pass

        def op_new(e):
            e.ret(e.alloc(e.arg(0)))

        def memcpy(e):
            e.write(e.arg(0), e.read(e.arg(1), e.arg(2)))
            e.ret(e.arg(0))

        def memset(e):
            e.write(e.arg(0), bytes([e.arg(1) & 0xff]) * e.arg(2))
            e.ret(e.arg(0))

        def strlen(e):
            e.ret(len(e.cstr(e.arg(0))))

        def compare_cstr(e):
            a, b = e.string_get(e.arg(0)), e.cstr(e.arg(1))
            e.ret(0 if a == b else (-1 if a < b else 1))

        def plus_cstr_str(e):  # operator+(char const*, string const&) -> ret in rdi
            e.string_set(e.arg(0), e.cstr(e.arg(1)) + e.string_get(e.arg(2)))
            e.ret(e.arg(0))

        def plus_str_cstr(e):  # operator+(string const&, char const*)
            e.string_set(e.arg(0), e.string_get(e.arg(1)) + e.cstr(e.arg(2)))
            e.ret(e.arg(0))

        s('_ZNSsC1EPKcRKSaIcE', str_from_cstr)
        s('_ZNSsC2EPKcRKSaIcE', str_from_cstr)
        s('_ZNSsC1ERKSs', str_copy)
        s('_ZNSsC2ERKSs', str_copy)
        s('_ZNSsC1ERKSsmm', str_substr_ctor)
        s('_ZNSs6assignEPKc', assign_cstr)
        s('_ZNSs6assignEPKcm', assign_cstr_n)
        s('_ZNSs6assignERKSs', assign_str)
        s('_ZNSs6appendEPKc', append_cstr)
        s('_ZNSs6appendEPKcm', append_cstr_n)
        s('_ZNSs6appendERKSs', append_str)
        s('_ZNSs6insertEmPKcm', insert_cstr_n)
        s('_ZNSs9push_backEc', push_back)
        s('_ZNSs4swapERSs', swap)
        s('_ZNSs7reserveEm', reserve)
        s('_ZNSs12_M_leak_hardEv', noop)
        s('_ZNSs4_Rep10_M_destroyERKSaIcE', noop)
        s('_ZNSs4_Rep10_M_disposeERKSaIcE', noop)
        s('_ZNKSs7compareEPKc', compare_cstr)
        s('_ZStplIcSt11char_traitsIcESaIcEESbIT_T0_T1_EPKS3_RKS6_', plus_cstr_str)
        s('_ZStplIcSt11char_traitsIcESaIcEESbIT_T0_T1_ERKS6_PKS3_', plus_str_cstr)
        s('_Znwm', op_new)
        s('_Znam', op_new)
        s('_ZdlPv', noop)
        s('_ZdaPv', noop)
        s('memcpy', memcpy)
        s('memmove', memcpy)
        s('memset', memset)
        s('strlen', strlen)
