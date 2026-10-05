"""Directors and level logic, lifted from superhex::gamelogic (see symbolic.py).

gamelogic runs once per tick. For each stage it is walked with the stage's identity concrete and
the state the logic reads symbolic. The resulting tree splits into the part that runs on every
tick (the level's `on_tick`) and the part that runs when the wave timer has run out (the
director's `on_wave`). Field reads and writes and calls are then mapped onto the pack's variables
and actions, and the tree is restructured into `pick` (random draws) and `first` (if/else
ladders).

The mapping of fields and calls onto pack terms is the one piece of interpretation here; anything
the logic does that the mapping doesn't cover is an error rather than being dropped silently.
"""
import struct
from multiprocessing import Pool

from angr import claripy

import content as C
from symbolic import Lifter, Translator, draw_ranges, prune, tail_dup

# --- lifting -------------------------------------------------------------------------------------

BASE_CONCRETE = dict(zoom=40, nsides=6, override_hexagoner=-1, override_hexagonest=-1, cursong=-1,
                     expectedframedelta=1.0, rank=99, pausewaves=0)
INPUTS = dict(time=(0, 20000), wavecount=(0, 65535), speed=(0, 100), nsides=(3, 6), sidechange=(0, 4),
              shapewavecounter=(0, 100), rotmode=(-1, 9), hyperopening=(0, 1), pal=(0, 1000),
              palstate=(0, 2), wavetimer=(-100, 1000))
ENDING_INPUTS = dict(INPUTS, timelinestate=(0, 200), timelinedelay=(-100, 100), spinburst=(0, 1),
                     now_ms=(0, 1000000))
del ENDING_INPUTS['time']


# what each lift is: (concrete fields, symbolic inputs)
JOBS = {
    'hexagon': (dict(stage=0), INPUTS),
    'hexagoner': (dict(stage=1), INPUTS),
    'hexagonest': (dict(stage=2), INPUTS),
    'ending': (dict(stage=4, unlockevent=3), ENDING_INPUTS),
    # the finale: Hexagonest after the game is complete; the hit that starts it, and what follows
    'finale_hit': (dict(stage=2, postwin=1), dict(INPUTS, blocked=(0, 10))),
    'finale': (dict(stage=2, postwin=2), INPUTS),
    # the tutorial: its logic, and the input it waits for (superhex::gameinput)
    'tutorial': (dict(stage=0, tutorial=1, tutorialflag=0),
                 dict(INPUTS, tutorialstate=(0, 10), tutorialtimer=(-10, 1000), menuslide=(-100, 100))),
    'tutorial_input': (dict(stage=0, tutorial=1, tutorialflag=0),
                       dict(tutorialstate=(0, 10), tutorialtimer=(-10, 1000), in_left=(0, 1), in_right=(0, 1))),
    # dying in the tutorial: once the camera has pulled back, it starts again
    'tutorial_death': (dict(stage=0, tutorial=1, tutorialflag=0, gameovertimer=100.0, zoom=150), {}),
}
METHOD = {'tutorial_input': '_ZN8superhex9gameinputEv'}


def lift_raw(args):
    binary, job = args
    concrete, inputs = JOBS[job]
    L = Lifter(binary, METHOD.get(job, '_ZN8superhex9gamelogicEv'), C.SUPERHEX_SIZE, C.SUPERHEX_FIELDS)
    L.handlers['_ZN9gameclass17getbesttime_stageEv'] = lambda st: 10 ** 6  # no new record this tick
    L.handlers['_Z22ofGetElapsedTimeMillisv'] = lambda st: 0
    L.handlers['_ZN8superhex14generickeypollEv'] = lambda st: None  # input is symbolic
    st = L.new_state(dict(BASE_CONCRETE, **concrete), inputs)
    base = list(st.solver.constraints)
    ir, _ = L.walk(st)
    ir = tail_dup(ir)
    ir = prune(ir, base + draw_ranges(ir))
    T = Translator(dict(inputs, timeline_ms=(0, 1000000), draw=(0, 1000)), derived=[
        ('sides_after_morph', ['nsides', 'sidechange'], lambda n, sc: claripy.If(sc == 1, n - 1, n))])
    return job, T.ast(ir)


def lift_all(binary):
    """{level logic name: (on_tick, on_wave)}, and the finale's on_hit."""
    with Pool(len(JOBS)) as pool:
        raw = dict(pool.map(lift_raw, [(binary, j) for j in JOBS]))
    out = {name: convert(raw[name]) for name in JOBS if not name.startswith(('finale_hit', 'tutorial'))}
    tick, wave = convert(raw['tutorial'], tutorial=True)
    tutorial = {'on_tick': convert_input(raw['tutorial_input']) + tick, 'on_wave': wave,
                'on_death': convert_input(raw['tutorial_death'])}
    return out, finale_hit(raw['finale_hit']), tutorial


def finale_hit(ast):
    """What runs when the player is hit in the post-win Hexagonest: the branch that moves on to
    the finale (postwin = 2)."""
    found = []

    def walk(acts):
        for x in acts:
            if 'if' in x:
                for k in ('then', 'else'):
                    if {'set': ['postwin', 2]} in x.get(k, []):
                        found.append(x[k])
                    walk(x.get(k, []))
    walk(ast)
    if len(found) != 1:
        raise LogicError('expected one hit branch, found %d' % len(found))
    return finish(actions(found[0], False))


# --- vocabulary ----------------------------------------------------------------------------------

VARS = {'wavecount': 'wave', 'time': 'time', 'speed': 'speed', 'nsides': 'sides', 'pal': 'palette',
        'sides_after_morph': 'sides_after_morph'}
BOOL_VARS = {'palstate': 'palette_fading', 'sidechange': 'morphing', 'spinburst': 'spin_burst_active',
             'in_left': 'left', 'in_right': 'right'}
COUNTERS = {'shapewavecounter': 'shape', 'hyperopening': 'hyper_opening', 'timelinestate': 'step',
            'timelinedelay': 'step_delay'}
# the tutorial's own state; its text slide paces it, so there it is a counter too
# how the ending hands over to the game-complete screen
END_SEQUENCE = {'unlockevent': 2, 'blocked': 5, 'stage': 2}
TUTORIAL_COUNTERS = {'tutorialstate': 'step', 'tutorialtimer': 'step_timer', 'menuslide': 'slide'}
counters_in_use = COUNTERS
FLOAT_FIELDS = {'time', 'speed', 'shapewavecounter', 'wavetimer', 'pulse', 'sidechangefreeze', 'fadetimer',
                'timelinedelay', 'blocked', 'tutorialtimer', 'menuslide'}
# engine bookkeeping: the clock, scratch values, fades, the wave counter and timer
IGNORED_SETS = {'time', 'temp', 'fade', 'fadestate', 'speedramp', 'palstate', 'unusedcc', 'timeline_ms', 'wavecount',
                'postwin', 'gameovertimer', 'menuslide', 'zoom'}
IGNORED_CALLS = {'_ZN10scoreclass14updatescoreapiEv', '_ZN9gameclass19updatevisualeffectsER9helpclass',
                 '_ZN9gameclass11setbesttimeEi', '_ZN10musicclass6playefEi', '_ZN9gameclass7cleanupEv',
                 '_ZN13graphicsclass9updatepalEi', '_ZN13graphicsclass6setpalEib', '_ZN9gameclass9savescoreEv',
                 '_ZN13graphicsclass6detachEv', '_ZN9gameclass7restartEv'}


class LogicError(Exception):
    pass


def f32(v):
    return struct.unpack('<f', struct.pack('<I', v & 0xffffffff))[0]


def num(x):
    return int(x) if x == int(x) else x


def is_draw(x):
    return isinstance(x, str) and x.startswith('draw')


def expr(e):
    if e is None:
        raise LogicError('untranslatable expression')
    if isinstance(e, str):
        if e in VARS:
            return VARS[e]
        if e in counters_in_use:
            return counters_in_use[e]
        if is_draw(e):
            return e
        raise LogicError('unmapped variable %s' % e)
    if isinstance(e, list):
        op = e[0]
        if op in ('==', '!=') and isinstance(e[1], str) and e[1] in BOOL_VARS and e[2] == 0:
            return ['not', BOOL_VARS[e[1]]] if op == '==' else BOOL_VARS[e[1]]
        if op in ('<=', '<', '>', '>=') and e[1] in ('timeline_ms', 'now_ms'):
            return elapsed_cmp(op, e[2])
        return [op] + [expr(a) for a in e[1:]]
    return e


def elapsed_cmp(op, ms):
    """The ending's clock was milliseconds since it began; express thresholds in ticks."""
    if op in ('>', '<='):
        ms += 1
        op = {'>': '>=', '<=': '<'}[op]
    ticks = ms * 60 / 1000
    if ticks != int(ticks):
        raise LogicError('ending threshold %d ms is not a whole tick' % ms)
    return [op, 'elapsed', int(ticks)]


def mentions(node, name):
    if isinstance(node, str):
        return node == name
    if isinstance(node, list):
        return any(mentions(x, name) for x in node)
    if isinstance(node, dict):
        return any(mentions(v, name) for v in node.values())
    return False


def actions(ast, in_wave):
    """Field/call-level statements -> pack actions."""
    out = []
    for x in ast:
        k = next(iter(x))
        if k == 'if':
            a, b = actions(x['then'], in_wave), actions(x.get('else', []), in_wave)
            if a == b:
                out.extend(a)
            else:
                node = {'if': expr(x['if']), 'then': a}
                if b:
                    node['else'] = b
                out.append(node)
        elif k == 'loop':
            # do { rotmode = (int)ofRandom(n) + base; } while (rotmode == old);
            body = x['loop']
            if not (len(body) == 3 and 'draw' in body[0] and body[1].get('set', [None])[0] == 'rotmode'
                    and 'if' in body[2] and body[2]['then'] == [{'continue': True}]):
                raise LogicError('unrecognised loop %r' % body)
            d, n = body[0]['draw']
            base = rotation_base(body[1]['set'][1], d)
            out.append({'reroll_rotation': list(range(base, base + n))})
        elif k == '_advance':
            out.append({'advance_waves': True})
        elif k == 'draw':
            out.append(x)
        elif k == 'set':
            out.extend(write(out, x['set'][0], x['set'][1], in_wave))
        elif k == 'call':
            out.extend(call(*x['call']))
        else:
            raise LogicError('unexpected statement %r' % x)
    ends = [a for a in out if '_end' in a]
    if ends:
        if sorted(tuple(a['_end']) for a in ends) != sorted(END_SEQUENCE.items()):
            raise LogicError('unrecognised end of sequence %r' % ends)
        at = out.index(ends[0])
        out = [a for a in out if '_end' not in a]
        out.insert(at, {'end_sequence': True})
    # tutorial = false, tutorialflag = 1: the tutorial is over, and won't be shown again
    ends = [a for a in out if '_end_tutorial' in a]
    if ends:
        if sorted(a['_end_tutorial'] for a in ends) != ['tutorial', 'tutorialflag']:
            raise LogicError('unrecognised end of tutorial %r' % ends)
        at = out.index(ends[0])
        out = [a for a in out if '_end_tutorial' not in a]
        out.insert(at, {'end_tutorial': True})
    return out


def write(out, field, v, in_wave):
    if field in counters_in_use:
        if v is None:
            raise LogicError('untranslatable value written to %s' % field)
        if field in FLOAT_FIELDS and isinstance(v, int):
            v = num(f32(v))
        return [{'set': [counters_in_use[field], expr(v)]}]
    if field in IGNORED_SETS:
        return []
    if v is None:
        raise LogicError('untranslatable value written to %s' % field)
    if field in FLOAT_FIELDS and isinstance(v, int):
        v = num(f32(v))
    if field == 'speed':
        return [{'set': ['speed', expr(v)]}]
    if field == 'rotmode':
        if isinstance(v, int):
            return [{'rotation': v}]
        d, n = take_draw(out, v)
        base = rotation_base(v, d)
        return [{'random_rotation': list(range(base, base + n))}]
    if field == 'pulse':
        # positive constants are the director's kicks; the per-tick beat pulse is the engine's
        return [{'pulse': v}] if isinstance(v, (int, float)) and v > 0 else []
    if field == 'zoompulse' and v == 1:
        return [{'zoom_pulse': True}]
    if field == 'spinburst' and v == 1:
        return [{'spin_burst': True}]
    if field == 'wobble':
        d, n = take_draw(out, v)
        if v == ['+', d, 1] and n == 2:
            return [{'tilt': 'random'}]
    if field == 'sidechange' and v in (1, 2):
        return [{'morph': {1: 'shrink', 2: 'grow'}[v]}]
    if field == 'sidechangefreeze':
        return [{'freeze_walls': v}]
    if field == 'fadetimer':
        return [{'flash': v}]
    if field == 'wavetimer' and in_wave and isinstance(v, (int, float)):
        return [{'delay': v}]
    if field == 'blocked' and v == 0:
        return []  # the ending can't be hit, which is the engine's business
    if field in END_SEQUENCE:
        return [{'_end': [field, v]}]
    if (field, v) in (('tutorial', 0), ('tutorialflag', 1)):
        return [{'_end_tutorial': field}]
    raise LogicError('unmapped write %s := %r' % (field, v))


def call(name, arg):
    if name in IGNORED_CALLS:
        return []
    if name == '_ZN9gameclass12generatewaveEi':
        return [{'pattern': str(arg)}]
    if name == '_ZN13graphicsclass9changepalEi':
        return [{'palette': expr(arg)}]
    if name == '_ZN9gameclass13changetostageEiR13graphicsclassR10musicclass':
        return [{'switch_level': {'changetostage': arg}}]  # resolved by the extractor (levels.py)
    if name == '_ZN9gameclass9seekangleEi':
        return [{'camera': {'lean': arg}}]
    if name == '_ZN9gameclass10otisrotateEv':
        return [{'camera': {'sway': True}}]
    if name == '_ZN9gameclass14nullotisrotateEv':
        return [{'camera': {'sway': False}}]
    if name == '_ZN10musicclass4playEi':
        return [{'music': 'track%d' % arg}]
    if name == '_ZN10musicclass4stopEv':
        return [{'music': None}]
    if name == '_ZN9gameclass12clearenemiesEv':
        return [{'clear_walls': True}]
    raise LogicError('unmapped call %s(%r)' % (name, arg))


def rotation_base(v, d):
    if v == d:
        return 0
    if isinstance(v, list) and v[0] == '+' and v[1] == d and isinstance(v[2], int):
        return v[2]
    raise LogicError('rotation expression %r' % v)


def take_draw(out, v):
    for j in range(len(out) - 1, -1, -1):
        if 'draw' in out[j] and mentions(v, out[j]['draw'][0]):
            return out.pop(j)['draw']
    raise LogicError('no draw for %r' % v)


# --- random draws -> pick ------------------------------------------------------------------------

def thresholds(node, d, n):
    cuts = set()

    def cond(c):
        if isinstance(c, list):
            if c[0] in ('<', '<=', '>', '>=', '==', '!=') and c[1] == d and isinstance(c[2], int):
                k = c[2]
                cuts.update({'<': [k], '<=': [k + 1], '>': [k + 1], '>=': [k], '==': [k, k + 1],
                             '!=': [k, k + 1]}[c[0]])
            else:
                for a in c[1:]:
                    cond(a)

    def walk(x):
        if isinstance(x, dict):
            if 'if' in x:
                cond(x['if'])
            for v in x.values():
                walk(v)
        elif isinstance(x, list):
            for v in x:
                walk(v)
    walk(node)
    return sorted(c for c in cuts if 0 < c < n)


def with_draw(c, d, val):
    """Evaluate the parts of condition `c` that test draw `d`."""
    if not isinstance(c, list):
        if c == d:
            raise LogicError('draw used as a value')
        return c
    op = c[0]
    if op in ('<', '<=', '>', '>=', '==', '!=') and c[1] == d:
        return int({'<': val < c[2], '<=': val <= c[2], '>': val > c[2], '>=': val >= c[2],
                    '==': val == c[2], '!=': val != c[2]}[op])
    args = [with_draw(a, d, val) for a in c[1:]]
    if op == 'not' and isinstance(args[0], int):
        return int(not args[0])
    if op in ('and', 'or'):
        unit, zero = (1, 0) if op == 'and' else (0, 1)
        if zero in args:
            return zero
        args = [a for a in args if a != unit]
        return unit if not args else args[0] if len(args) == 1 else [op] + args
    return [op] + args


def specialise(acts, d, val):
    out = []
    for a in acts:
        if 'if' in a:
            c = with_draw(a['if'], d, val)
            if c in (0, 1):
                out.extend(specialise(a['then'] if c else a.get('else', []), d, val))
            else:
                node = dict(a, **{'if': c, 'then': specialise(a['then'], d, val)})
                if 'else' in a:
                    node['else'] = specialise(a['else'], d, val)
                out.append(node)
        elif mentions(a, d):
            raise LogicError('draw %s used in %r' % (d, a))
        else:
            out.append(recurse(a, lambda l: specialise(l, d, val)))
    return out


def recurse(a, fn):
    a = dict(a)
    for k in ('then', 'else'):
        if k in a:
            a[k] = fn(a[k])
    if 'pick' in a:
        a['pick'] = [[w, fn(e)] for w, e in a['pick']]
    if 'first' in a:
        a['first'] = [dict(r, do=fn(r['do'])) for r in a['first']]
    return a


def picks(acts):
    """A draw and the statements that test it become a pick over the draw's outcomes, with
    entries in draw order. Draws whose result is never used are dropped."""
    acts = [recurse(a, picks) for a in acts]
    out = []
    i = 0
    while i < len(acts):
        a = acts[i]
        if 'draw' not in a:
            out.append(a)
            i += 1
            continue
        d, n = a['draw']
        last = max([j for j in range(i + 1, len(acts)) if mentions(acts[j], d)], default=None)
        if last is None:
            i += 1
            continue
        span = acts[i + 1:last + 1]
        cuts = [0] + thresholds(span, d, n) + [n]
        entries = []
        for lo, hi in zip(cuts, cuts[1:]):
            body = picks(specialise(span, d, lo))
            if entries and entries[-1][1] == body:
                entries[-1][0] += hi - lo
            else:
                entries.append([hi - lo, body])
        out.append({'pick': entries})
        i = last + 1
    return out


# --- if/else ladders -> first --------------------------------------------------------------------

NEG = {'<': '>=', '<=': '>', '>': '<=', '>=': '<', '==': '!=', '!=': '=='}


def negate(c):
    if isinstance(c, list) and c[0] in NEG:
        return [NEG[c[0]]] + c[1:]
    if isinstance(c, list) and c[0] == 'not':
        return c[1]
    return ['not', c]


def is_chain(l):
    return len(l) == 1 and ('if' in l[0] or 'first' in l[0])


def ladders(acts):
    out = []
    for a in acts:
        a = recurse(a, ladders)
        while 'if' in a and 'else' not in a and len(a['then']) == 1 and 'if' in a['then'][0] \
                and 'else' not in a['then'][0]:
            inner = a['then'][0]
            conds = (a['if'][1:] if isinstance(a['if'], list) and a['if'][0] == 'and' else [a['if']]) + [inner['if']]
            a = {'if': ['and'] + conds, 'then': inner['then']}
        if 'if' in a and not a['then'] and a.get('else'):
            a = {'if': negate(a['if']), 'then': a['else']}
        if 'if' in a and a.get('else') and is_chain(a['then']) and \
                (not is_chain(a['else']) or ('if' in a['else'][0] and 'else' not in a['else'][0])):
            a = {'if': negate(a['if']), 'then': a['else'], 'else': a['then']}
        if 'if' in a and a.get('else') and is_chain(a['else']):
            rest = a['else'][0]
            rules = [{'when': a['if'], 'do': a['then']}]
            if 'first' in rest:
                rules += rest['first']
            else:
                rules.append({'when': rest['if'], 'do': rest['then']})
                if 'else' in rest:
                    rules.append({'do': rest['else']})
            a = {'first': rules}
        out.append(a)
    return out


def compact(acts):
    """Pick entries holding a single action are written as that action, empty ones as null."""
    out = []
    for a in acts:
        a = recurse(a, compact)
        if 'pick' in a:
            a = {'pick': [[w, b[0] if len(b) == 1 else (b or None)] for w, b in a['pick']]}
        out.append(a)
    return out


# --- per-tick part and wave block ----------------------------------------------------------------

def canon(node):
    """Rename draws in order of appearance, so that equal logic compares equal."""
    names = {}

    def walk(x):
        if is_draw(x):
            return names.setdefault(x, 'draw#%d' % len(names))
        if isinstance(x, list):
            return [walk(v) for v in x]
        if isinstance(x, dict):
            return {k: walk(v) for k, v in x.items()}
        return x
    return walk(node)


DECREMENT = {'set': ['wavetimer', ['-', 'wavetimer', 1]]}


def wave_block(ast):
    """Find the wave block: what runs once the wave timer has counted down (the compiler may have
    duplicated it across the test). Returns the statements with the block replaced by an
    advance_waves marker, where it was, and its body."""
    for i, x in enumerate(ast):
        if 'if' not in x:
            continue
        if mentions(x['if'], 'wavetimer'):
            bodies = []

            def collect(branch):
                branch = [s for s in branch if s != DECREMENT]
                if not branch:
                    return
                if branch[0] == {'set': ['wavetimer', 0]}:
                    bodies.append(branch[1:])
                elif len(branch) == 1 and 'if' in branch[0] and mentions(branch[0]['if'], 'wavetimer'):
                    collect(branch[0]['then'])
                    collect(branch[0].get('else', []))
                else:
                    raise LogicError('unexpected statements around the wave block')
            collect(x['then'])
            collect(x.get('else', []))
            if not bodies or any(canon(b) != canon(bodies[0]) for b in bodies):
                raise LogicError('wave blocks differ')
            body = bodies[0]
            if body and body[-1] == {'set': ['wavecount', ['+', 'wavecount', 1]]}:
                body = body[:-1]  # the engine counts waves
            rest = [s for s in ast[:i] if s != DECREMENT] + [ADVANCE] + ast[i + 1:]
            return rest, body
        for key in ('then', 'else'):
            found = wave_block(x.get(key, []))
            if found:
                branch, body = found
                return ast[:i] + [dict(x, **{key: branch})] + ast[i + 1:], body
    return None


ADVANCE = {'_advance': True}


def convert(ast, tutorial=False):
    global counters_in_use
    counters_in_use = TUTORIAL_COUNTERS if tutorial else COUNTERS
    try:
        found = wave_block(ast)
        if not found:
            raise LogicError('no wave block')
        tick, wave = found
        return finish(actions(tick, False)), finish(actions(wave, True))
    finally:
        counters_in_use = COUNTERS


def convert_input(ast):
    """Logic in the input handler (the tutorial's wait for both directions)."""
    global counters_in_use
    counters_in_use = TUTORIAL_COUNTERS
    try:
        return finish(actions(ast, False))
    finally:
        counters_in_use = COUNTERS


def finish(acts):
    return compact(ladders(picks(acts)))


def counters(*trees):
    """Counters a level's logic uses."""
    found = set()

    def walk(x):
        if isinstance(x, dict):
            if 'set' in x and x['set'][0] in set(COUNTERS.values()) | set(TUTORIAL_COUNTERS.values()):
                found.add(x['set'][0])
            for v in x.values():
                walk(v)
        elif isinstance(x, list):
            for v in x:
                walk(v)
    walk(list(trees))
    return sorted(found)
