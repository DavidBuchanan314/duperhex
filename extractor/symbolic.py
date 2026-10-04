"""Structural lifting of compiled game logic into condition/effect trees, with angr.

A function is walked block by block with angr's symbolic engine. Fields of the game object that
the logic reads are symbols; every write to a field is recorded as an effect, and calls are
replaced by hooks that record them. At a conditional branch both sides are walked separately up
to the branch's immediate post-dominator, where the two states are merged again (differing values
become if-then-else expressions), so the result keeps the shape of the source's if/else nesting
instead of enumerating paths. Loops are walked once and recorded as such.

Field symbols are versioned: a write gives the field a new symbol, so a condition that reads the
field after a write is expressed in terms of the field as it then stands, which is what an
interpreter of the result will see.

The IR produced by `Lifter.walk` is a list of statements:
    ('set', field, value)        field write
    ('call', name, arg)          hooked call (first integer argument)
    ('draw', symbol, n)          random draw in [0, n)
    ('mod', symbol, x, m)        symbol stands for superRemainder(x, m)
    ('if', guard, then, else)    branch
    ('loop', body), ('continue',)
`Translator` turns guards and values into pack expressions.
"""
import itertools
import logging
import re
import struct

import angr
import networkx as nx
from angr import claripy

logging.getLogger('angr').setLevel(logging.ERROR)
logging.getLogger('cle').setLevel(logging.ERROR)
logging.getLogger('pyvex').setLevel(logging.ERROR)

OBJ = 0x1000_0000      # address of the object the lifted method runs on
STACK = 0x2000_0000
EXIT = 0x3000_0000     # return address: the walk ends here
SIZE = {'i': 4, 'f': 4, 'd': 8, 'b': 1, 'l': 8}
REGS = ['rax', 'rbx', 'rcx', 'rdx', 'rsi', 'rdi', 'rbp', 'r8', 'r9', 'r10', 'r11', 'r12', 'r13', 'r14',
        'r15'] + ['xmm%d' % i for i in range(16)]


class LiftError(Exception):
    pass


class Lifter:
    """Walks one method. `fields`: name -> (offset in the object, kind), kind one of i f d b l."""

    def __init__(self, path, method, obj_size, fields):
        self.p = angr.Project(path, auto_load_libs=False)
        sym = self.p.loader.find_symbol(method)
        self.fn = sym.rebased_addr
        self.obj_size = obj_size
        self.fields = fields
        self.byaddr = {OBJ + off: name for name, (off, _) in fields.items()}
        cfg = self.p.analyses.CFGFast(regions=[(self.fn, self.fn + sym.size)], normalize=True,
                                      function_starts=[self.fn])
        self.func = cfg.kb.functions[self.fn]
        self.names = {}
        for s in self.p.loader.main_object.symbols:
            if s.rebased_addr:
                self.names.setdefault(s.rebased_addr, s.name)
        for f in cfg.kb.functions.values():
            self.names.setdefault(f.addr, f.name)
        self._graph()
        self._hook_calls()
        self.nver = itertools.count()
        self.quiet = False
        self.handlers = {}

    # --- control flow ---------------------------------------------------------------------------
    def _graph(self):
        g = nx.DiGraph()
        self.blocks = {}
        for b in self.func.blocks:
            self.blocks[b.addr] = b.size
            g.add_node(b.addr)
        for a, b in self.func.graph.edges():
            g.add_edge(a.addr, b.addr)
        for site in self.func.get_call_sites():
            ret = site + self.blocks[site]
            if ret in self.blocks:
                g.add_edge(site, ret)
        for n in [n for n in g.nodes if g.out_degree(n) == 0]:
            g.add_edge(n, EXIT)
        self.ipdom = nx.immediate_dominators(g.reverse(copy=True), EXIT)
        dom = nx.immediate_dominators(g, self.fn)

        def dominates(a, b):
            while a != b:
                if dom.get(b, b) == b:
                    return False
                b = dom[b]
            return True
        loops = {}
        for a, b in g.edges:
            if b != EXIT and dominates(b, a):
                body = loops.setdefault(b, {b})
                todo = [a]
                while todo:
                    n = todo.pop()
                    if n not in body:
                        body.add(n)
                        todo.extend(g.predecessors(n))
        self.loop_exit = {}
        for h, body in loops.items():
            x = h
            while x in body:
                x = self.ipdom[x]
            self.loop_exit[h] = x
        self.starts = sorted(self.blocks)

    def node_of(self, addr):
        if addr in self.blocks:
            return addr
        for s in self.starts:
            if s < addr < s + self.blocks[s]:
                return s
        raise LiftError('address %#x is not in the function' % addr)

    # --- calls ----------------------------------------------------------------------------------
    def _hook_calls(self):
        lifter = self
        rand_returns = []
        for site in self.func.get_call_sites():
            target = self.func.get_call_target(site)
            name = self.names.get(target, hex(target))
            if name == '_Z8ofRandomf':
                rand_returns.append(site + self.blocks[site])
            if self.p.is_hooked(target):
                continue

            class Hook(angr.SimProcedure):
                fname = name

                def run(self):
                    return lifter.on_call(self.state, self.fname)
            self.p.hook(target, Hook())
        # (int)ofRandom(n): the conversion right after the call yields the draw itself
        for ret in rand_returns:
            insn = self.p.factory.block(ret).capstone.insns[0]
            if insn.mnemonic == 'cvttss2si' and insn.op_str.endswith(', xmm0'):
                reg = insn.op_str.split(', ')[0]

                def convert(state, reg=reg):
                    setattr(state.regs, reg, state.globals['draw'])
                self.p.hook(ret, convert, length=insn.size)

    def log(self, state, *rec):
        state.globals['fx'] = state.globals['fx'] + (rec,)

    def on_call(self, state, name):
        if name == '_Z8ofRandomf':
            n = struct.unpack('<f', struct.pack('<I', state.solver.eval(state.regs.xmm0[31:0])))[0]
            if n != int(n):
                raise LiftError('ofRandom(%r)' % n)
            d = claripy.BVS('draw%d' % next(self.nver), 32, explicit_name=True)
            state.add_constraints(claripy.SGE(d, 0), claripy.SLT(d, int(n)))
            state.globals['draw'] = d
            self.log(state, 'draw', d, int(n))
            state.regs.xmm0 = claripy.BVS('unused_draw', 128)
            return None
        if name == '_Z14superRemainderdi':
            m = claripy.BVS('mod%d' % next(self.nver), 32, explicit_name=True)
            self.log(state, 'mod', m, state.regs.xmm0[63:0], state.regs.edi)
            return m
        if name in self.handlers:
            return self.handlers[name](state)
        self.log(state, 'call', name, state.regs.esi)
        return None

    # --- state ----------------------------------------------------------------------------------
    def new_state(self, concrete, symbolic):
        """concrete: field -> value; symbolic: field -> (lo, hi) range of the input symbol."""
        st = self.p.factory.blank_state(addr=self.fn, add_options={
            angr.options.SYMBOL_FILL_UNCONSTRAINED_MEMORY, angr.options.SYMBOL_FILL_UNCONSTRAINED_REGISTERS,
            angr.options.LAZY_SOLVES})
        st.regs.rdi = OBJ
        st.regs.rsp = STACK
        st.memory.store(STACK, claripy.BVV(EXIT, 64), endness='Iend_LE')
        st.memory.store(OBJ, claripy.BVV(0, self.obj_size * 8))
        st.globals['fx'] = ()
        st.globals['ver'] = {}
        for name, v in concrete.items():
            off, k = self.fields[name]
            if k == 'f':
                bv = claripy.BVV(struct.unpack('<I', struct.pack('<f', v))[0], 32)
            elif k == 'd':
                bv = claripy.BVV(struct.unpack('<Q', struct.pack('<d', v))[0], 64)
            else:
                bv = claripy.BVV(v & ((1 << (8 * SIZE[k])) - 1), 8 * SIZE[k])
            st.memory.store(OBJ + off, bv, endness='Iend_LE')
        for name, (lo, hi) in symbolic.items():
            s = self.new_version(st, name)
            st.memory.store(OBJ + self.fields[name][0], s, endness='Iend_LE')
            if self.fields[name][1] == 'f':
                f = s.raw_to_fp()
                st.add_constraints(claripy.fpGEQ(f, claripy.FPV(lo, claripy.FSORT_FLOAT)),
                                   claripy.fpLEQ(f, claripy.FPV(hi, claripy.FSORT_FLOAT)))
            else:
                st.add_constraints(claripy.SGE(s, lo), claripy.SLE(s, hi))
        st.inspect.b('mem_write', when=angr.BP_BEFORE, action=self.on_write)
        return st

    def new_version(self, st, name):
        s = claripy.BVS('%s#%d' % (name, next(self.nver)), 8 * SIZE[self.fields[name][1]], explicit_name=True)
        st.globals['ver'] = dict(st.globals['ver'], **{name: s})
        return s

    def on_write(self, st):
        if self.quiet:
            return
        addr = st.inspect.attrs.mem_write_address
        if not isinstance(addr, int):
            if addr.symbolic:
                return
            addr = st.solver.eval(addr)
        name = self.byaddr.get(addr)
        if name is None:
            return
        val = st.inspect.attrs.mem_write_expr
        self.log(st, 'set', name, val)
        if name in st.globals['ver']:
            s = self.new_version(st, name)
            st.inspect.attrs.mem_write_expr = s
            subst = dict(st.globals['subst'])
            subst[val] = s
            if val.op == 'fpToIEEEBV':
                subst[val.args[0]] = s.raw_to_fp()
            st.globals['subst'] = subst

    # --- walking --------------------------------------------------------------------------------
    def step(self, st):
        st.globals['fx'] = ()
        st.globals['subst'] = {}
        if self.p.is_hooked(st.addr):
            succ = self.p.factory.successors(st)
        else:
            node = self.node_of(st.addr)
            succ = self.p.factory.successors(st, size=node + self.blocks[node] - st.addr)
        out = succ.flat_successors
        for s in out:
            # values computed before a write, still held in registers or used by the branch,
            # are re-expressed through the field's new version
            sub = [(k, x) for k, x in s.globals['subst'].items() if k.symbolic and k.size() == x.size()]
            if sub:
                for r in REGS:
                    v = nv = getattr(s.regs, r)
                    for k, x in sub:
                        if k.size() <= nv.size():
                            nv = nv.replace(k, x)
                    if nv is not v:
                        setattr(s.regs, r, nv)
                g = s.scratch.guard
                for k, x in sub:
                    g = g.replace(k, x)
                s.scratch.guard = g
        return out

    def walk(self, st, stop=EXIT, loops=frozenset()):
        """Walk from `st` to address `stop`. Returns (statements, state at stop or None)."""
        out = []
        while True:
            if st.addr == stop:
                return out, st
            if st.addr == EXIT:
                return out, None
            if st.addr in self.loop_exit and st.addr not in loops:
                body, st = self.walk(st, self.loop_exit[st.addr], loops | {st.addr})
                out.append(('loop', body))
                if st is None:
                    return out, None
                continue
            here = st.addr
            succ = self.step(st)
            if not succ:
                return out, None
            out.extend(succ[0].globals['fx'])
            if len(succ) == 1:
                st = succ[0]
                if st.addr in loops:
                    out.append(('continue',))
                    return out, None
                continue
            if len(succ) != 2:
                raise LiftError('%d-way branch at %#x' % (len(succ), here))
            join = self.ipdom[self.node_of(here)]
            guard = succ[0].scratch.guard
            res = [([('continue',)], None) if s.addr in loops else self.walk(s, join, loops) for s in succ]
            out.append(('if', guard, res[0][0], res[1][0]))
            st = self.merge(res[0][1], res[1][1], guard)
            if st is None:
                return out, None

    def merge(self, a, b, guard):
        if a is None or b is None:
            return a or b
        self.quiet = True
        m, _, _ = a.merge(b, merge_conditions=[[guard], [claripy.Not(guard)]])
        va, vb = a.globals['ver'], b.globals['ver']
        ver = dict(va)
        for name in va:
            if va[name] is vb[name]:
                continue
            # written on either side: from here on it is simply "the field"
            s = ver[name] = claripy.BVS('%s#%d' % (name, next(self.nver)), va[name].size(), explicit_name=True)
            m.memory.store(OBJ + self.fields[name][0], s, endness='Iend_LE')
            for r in REGS:
                ra, rb = getattr(a.regs, r), getattr(b.regs, r)
                n = s.size()
                if ra.size() >= n and ra[n - 1:0] is va[name] and rb[n - 1:0] is vb[name]:
                    setattr(m.regs, r, s.zero_extend(ra.size() - n) if ra.size() > n else s)
        m.globals['ver'] = ver
        m.solver.reload_solver([c for c in a.solver.constraints if any(c is d for d in b.solver.constraints)])
        self.quiet = False
        return m


# --- IR passes -----------------------------------------------------------------------------------

def subterms(e):
    seen = set()
    todo = [e]
    while todo:
        x = todo.pop()
        h = x.hash()
        if h in seen:
            continue
        seen.add(h)
        yield x
        todo.extend(a for a in x.args if hasattr(a, 'hash'))


def leaves(e):
    return [x for x in subterms(e) if not any(hasattr(a, 'hash') for a in x.args)]


def draws_in(ir):
    out = set()
    for x in ir:
        if x[0] == 'draw':
            out.add(x[1].args[0])
        elif x[0] == 'if':
            out |= draws_in(x[2]) | draws_in(x[3])
        elif x[0] == 'loop':
            out |= draws_in(x[1])
    return out


def uses(ir, names):
    for x in ir:
        if x[0] == 'if':
            if x[1].variables & names or uses(x[2], names) or uses(x[3], names):
                return True
        elif x[0] == 'loop':
            if uses(x[1], names):
                return True
        elif x[0] != 'draw' and any(a.variables & names for a in x[1:] if hasattr(a, 'variables')):
            return True
    return False


def assume(ir, g):
    """Decide if-then-else values in `ir` whose conditions follow from `g`."""
    memo = {}

    def implied(c):
        h = c.hash()
        if h not in memo:
            memo[h] = None
            for val, cc in ((True, claripy.Not(c)), (False, c)):
                s = claripy.Solver()
                s.add(g)
                s.add(cc)
                if not s.satisfiable():
                    memo[h] = val
        return memo[h]

    def fix(e):
        if not hasattr(e, 'variables') or not (e.variables & g.variables):
            return e
        for sub in list(subterms(e)):
            if sub.op == 'If':
                r = implied(sub.args[0])
                if r is not None:
                    e = e.replace(sub.args[0], claripy.BoolV(r))
        return claripy.simplify(e)
    out = []
    for x in ir:
        if x[0] == 'if':
            out.append(('if', fix(x[1]), assume(x[2], g), assume(x[3], g)))
        elif x[0] == 'loop':
            out.append(('loop', assume(x[1], g)))
        else:
            out.append(tuple(fix(a) for a in x))
    return out


def tail_dup(ir):
    """Where a branch makes a draw that later statements test through a merged value (e.g. a
    draw over 5 or 6 outcomes depending on time), move those statements into both branches."""
    out = []
    for i, x in enumerate(ir):
        if x[0] == 'if':
            ds = draws_in(x[2]) | draws_in(x[3])
            rest = ir[i + 1:]
            if ds and uses(rest, ds):
                out.append(('if', x[1], tail_dup(x[2] + assume(rest, x[1])),
                            tail_dup(x[3] + assume(rest, claripy.Not(x[1])))))
                return out
            out.append(('if', x[1], tail_dup(x[2]), tail_dup(x[3])))
        elif x[0] == 'loop':
            out.append(('loop', tail_dup(x[1])))
        else:
            out.append(x)
    return out


def prune(ir, base, path=()):
    """Drop branches that can't be taken given the input ranges and enclosing conditions."""
    def feasible(cs):
        s = claripy.Solver()
        for c in cs:
            s.add(c)
        return s.satisfiable()
    out = []
    for x in ir:
        if x[0] == 'if':
            g, ng = x[1], claripy.Not(x[1])
            t, f = feasible(base + list(path) + [g]), feasible(base + list(path) + [ng])
            if t and f:
                out.append(('if', g, prune(x[2], base, path + (g,)), prune(x[3], base, path + (ng,))))
            elif t:
                out.extend(prune(x[2], base, path + (g,)))
            elif f:
                out.extend(prune(x[3], base, path + (ng,)))
        elif x[0] == 'loop':
            out.append(('loop', prune(x[1], base, path)))
        else:
            out.append(x)
    return out


def draw_ranges(ir):
    out = []
    for x in ir:
        if x[0] == 'draw':
            out += [claripy.SGE(x[1], 0), claripy.SLT(x[1], x[2])]
        elif x[0] == 'if':
            out += draw_ranges(x[2]) + draw_ranges(x[3])
        elif x[0] == 'loop':
            out += draw_ranges(x[1])
    return out


# --- expressions ---------------------------------------------------------------------------------

class Unclean(Exception):
    pass


def symname(s):
    """version symbol 'speed#12' -> 'speed'; others (draws) keep their name"""
    return s.args[0].split('#')[0]


def signed(v, bits):
    return v - (1 << bits) if v >> (bits - 1) else v


def num(x):
    return int(x) if x == int(x) else x


def cmod(x, m):
    return -((-x) % m) if x < 0 else x % m


CMP = {'__eq__': '==', '__ne__': '!=', 'SLT': '<', 'SLE': '<=', 'SGT': '>', 'SGE': '>=',
       'fpLT': '<', 'fpLEQ': '<=', 'fpGT': '>', 'fpGEQ': '>=', 'fpEQ': '=='}
RELATIONS = (('==', lambda x, y: x == y, lambda x, y: x == y, claripy.fpEQ),
             ('!=', lambda x, y: x != y, lambda x, y: x != y, claripy.fpNEQ),
             ('<', lambda x, y: x < y, claripy.SLT, claripy.fpLT),
             ('<=', lambda x, y: x <= y, claripy.SLE, claripy.fpLEQ),
             ('>', lambda x, y: x > y, claripy.SGT, claripy.fpGT),
             ('>=', lambda x, y: x >= y, claripy.SGE, claripy.fpGEQ))
ARITH = {'__add__': '+', 'fpAdd': '+', '__sub__': '-', 'fpSub': '-', '__mul__': '*', 'fpMul': '*'}
CONVERSIONS = ('bvToFP', 'fpToFp', 'fpToFP', 'fpToIEEEBV', 'fpToSBV')


class Translator:
    """claripy expressions -> pack expressions over field names.

    Comparisons the compiler expressed through flag arithmetic, multiply-by-reciprocal remainders
    and similar are recognised by testing candidate forms on sample points, then proving the
    candidate equivalent with the solver over the variable's range.

    domains: field -> (lo, hi); derived: [(name, [field, ...], fn(*symbols) -> expr)] for values
    the logic computes from several fields (e.g. sides after a morph)."""

    def __init__(self, domains, derived=()):
        self.domains = domains
        self.derived = list(derived)
        self.terms = {}

    def term(self, e):
        op = e.op
        if op in ('BVS', 'FPS'):
            return self.terms.get(e.args[0]) or symname(e)
        if op == 'BVV':
            return signed(e.args[0], e.size())
        if op == 'FPV':
            return num(e.args[0])
        if op in CONVERSIONS or op.endswith('ToFP'):
            return self.term([a for a in e.args if hasattr(a, 'hash') and a.op != 'RM'][-1])
        if op == 'Extract':
            hi, lo, x = e.args
            if lo == 0 and x.op in ('SignExt', 'ZeroExt') and hi + 1 <= x.args[1].size():
                return self.term(x.args[1])
            raise Unclean(e)
        if op in ('SignExt', 'ZeroExt'):
            return self.term(e.args[1])
        if op in ARITH:
            args = [a for a in e.args if hasattr(a, 'hash') and a.op != 'RM']
            out = self.term(args[0])
            for a in args[1:]:
                b = self.term(a)
                if ARITH[op] == '+' and isinstance(b, (int, float)) and b < 0:
                    out = ['-', out, -b]
                else:
                    out = [ARITH[op], out, b]
            return out
        raise Unclean(e)

    def value(self, e):
        """A value: direct translation, or a recognised remainder form of one variable."""
        try:
            return self.term(e)
        except Unclean:
            pass
        syms = {x for x in leaves(e) if x.op == 'BVS'}
        if len(syms) != 1 or e.size() != 32:
            raise Unclean(e)
        v = syms.pop()
        var = self.term(v)
        pts = self.samples(v, False)
        got = []
        for x in pts:
            r = claripy.simplify(e.replace(v, claripy.BVV(x & 0xffffffff, 32)))
            got.append(signed(r.args[0], 32) if r.op == 'BVV' else None)
        consts = {signed(c.args[0], c.size()) for c in leaves(e) if c.op == 'BVV'}
        consts = sorted({c for c in consts | {-c for c in consts} if abs(c) < 1000} | {0})
        for a in consts:
            for m in range(2, 65):
                for b in consts:
                    if [cmod(x - a, m) + b for x in pts] == got and \
                            self.equiv(e, claripy.SMod(v - a, claripy.BVV(m, 32)) + b):
                        out = ['%', ['-', var, a] if a else var, m]
                        return ['+', out, b] if b else out
        raise Unclean(e)

    def cond(self, b):
        b = claripy.simplify(b)
        if b.is_true():
            return 1
        if b.is_false():
            return 0
        if b.op in ('And', 'Or'):
            return [b.op.lower()] + [self.cond(x) for x in b.args]
        if b.op == 'Not':
            return ['not', self.cond(b.args[0])]
        if b.op in CMP:
            try:
                return [CMP[b.op], self.term(b.args[0]), self.term(b.args[1])]
            except Unclean:
                pass
        syms = {x for x in leaves(b) if x.op == 'BVS'}
        if len({x.args[0] for x in syms}) == 1:
            return self.recognise(b, syms.pop())
        return self.recognise_derived(b, syms)

    def recognise(self, b, v):
        var = self.term(v)
        is_float = any(x.op == 'bvToFP' and x.args[0] is v for x in subterms(b))
        consts = sorted({signed(c.args[0], c.size()) for c in leaves(b) if c.op == 'BVV'} |
                        {num(c.args[0]) for c in leaves(b) if c.op == 'FPV'})
        fv = v.raw_to_fp()
        cands = []
        for c in consts:
            for cc in ((c,) if is_float else (c - 1, c, c + 1)):
                for o, py, ib, fb in RELATIONS:
                    if is_float:
                        mk = (lambda fb=fb, cc=cc: fb(fv, claripy.FPV(float(cc), claripy.FSORT_FLOAT)))
                    else:
                        mk = (lambda ib=ib, cc=cc: ib(v, claripy.BVV(cc & ((1 << v.size()) - 1), v.size())))
                    cands.append(([o, var, cc], (lambda x, py=py, cc=cc: py(x, cc)), mk))
        if not is_float:
            for m in range(2, 129):
                for o, py, ib, _ in RELATIONS[:2]:
                    cands.append(([o, ['%', var, m], 0], (lambda x, py=py, m=m: py(cmod(x, m), 0)),
                                  (lambda ib=ib, m=m: ib(claripy.SMod(v, claripy.BVV(m, v.size())),
                                                         claripy.BVV(0, v.size())))))
        pts = self.samples(v, is_float)
        truth = [self.eval_at(b, v, x, is_float) for x in pts]
        extra = [claripy.Not(claripy.fpIsNaN(fv))] if is_float else []
        for ast, py, mk in cands:
            if [bool(py(x)) for x in pts] == truth and self.equiv(b, mk(), extra):
                return ast
        raise Unclean(b)

    def recognise_derived(self, b, syms):
        byfield = {symname(x): x for x in syms}
        for name, fields, fn in self.derived:
            if set(fields) != set(byfield):
                continue
            d = claripy.BVS(name, 32, explicit_name=True)
            defn = d == fn(*[byfield[f] for f in fields])
            consts = sorted({signed(c.args[0], c.size()) for c in leaves(b) if c.op == 'BVV'})
            for c in consts:
                for cc in (c - 1, c, c + 1):
                    for o, _, ib, _ in RELATIONS:
                        if self.equiv(b, ib(d, claripy.BVV(cc & 0xffffffff, 32)), [defn]):
                            return [o, name, cc]
        raise Unclean(b)

    def domain(self, v):
        name = v.args[0]
        if '#' in name:
            return self.domains.get(symname(v), (-200, 20000))
        m = re.match(r'([a-z]+)\d+$', name)
        return self.domains.get(m.group(1) if m else name, (-200, 20000))

    def equiv(self, a, b, extra=()):
        """a == b for all inputs within their ranges."""
        s = claripy.Solver()
        for c in extra:
            s.add(c)
        for x in leaves(a):
            if x.op == 'BVS' and x.size() == 32 and not any(y.op == 'bvToFP' and y.args[0] is x for y in subterms(a)):
                lo, hi = self.domain(x)
                s.add(claripy.SGE(x, lo))
                s.add(claripy.SLE(x, hi))
        s.add(a != b)
        return not s.satisfiable()

    def samples(self, v, is_float):
        lo, hi = (int(x) for x in self.domain(v))
        pts = set(range(lo, min(hi, lo + 300) + 1)) | set(range(lo, hi + 1, max(1, (hi - lo) // 300))) | {hi}
        if is_float:
            pts |= {x + 0.5 for x in pts}
        return sorted(pts)

    def eval_at(self, b, v, x, is_float):
        if is_float:
            val = claripy.BVV(struct.unpack('<I', struct.pack('<f', x))[0], 32)
        else:
            val = claripy.BVV(int(x) & ((1 << v.size()) - 1), v.size())
        r = claripy.simplify(b.replace(v, val))
        if r.is_true() or r.is_false():
            return r.is_true()
        return bool(claripy.Solver().eval(r, 1)[0])

    def ast(self, ir):
        """IR -> statement dicts over field names: {'if','then','else'}, {'loop'}, {'continue'},
        {'draw': [name, n]}, {'set': [field, value]}, {'call': [name, arg]}. Values that can't be
        expressed become None; the vocabulary pass rejects any it needs."""
        out = []
        for x in ir:
            k = x[0]
            if k == 'if':
                a, b = self.ast(x[2]), self.ast(x[3])
                if a or b:
                    try:
                        c = self.cond(x[1])
                    except Unclean:
                        c = None  # rejected later unless the branches turn out to do nothing
                    node = {'if': c, 'then': a}
                    if b:
                        node['else'] = b
                    out.append(node)
            elif k == 'loop':
                out.append({'loop': self.ast(x[1])})
            elif k == 'continue':
                out.append({'continue': True})
            elif k == 'draw':
                out.append({'draw': [x[1].args[0], x[2]]})
            elif k == 'mod':
                self.terms[x[1].args[0]] = ['%', self.term(x[2]), self.term(x[3])]
            elif k in ('set', 'call'):
                try:
                    v = self.value(x[2])
                except Unclean:
                    v = None
                out.append({k: [x[1], v]})
        return out
