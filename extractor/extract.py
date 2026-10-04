#!/usr/bin/env python3
"""Extract an asset pack from an installed copy of Super Hexagon.

    extract.py GAME_DIR OUT.zip

GAME_DIR is the game's install directory (containing SuperHexagon and data/). Data that lives in
code is recovered by emulating the game's own functions (see emu.py), and the game logic by lifting
it symbolically (see logic.py). Struct layouts and the remaining tables in content.py are specific
to this build, hence the hash check.
"""
import hashlib
import json
import os
import struct
import sys
import zipfile
from collections import OrderedDict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from emu import Emu  # noqa: E402
import content as C  # noqa: E402
import levels as LV  # noqa: E402
import logic  # noqa: E402
import timeline  # noqa: E402

BINARY_SHA256 = '6f13c58d136b84df212cc23a560f7383e2f689005af18072e51d8a7439d2ba1d'  # Linux build 8838351
FORMAT = 1


def setup(e):
    """Stubs for engine functions the extracted code calls but that have no bearing on the data."""
    for name in ('_ZN5ofLogC1Ei', '_ZN5ofLogC1EiPKcz', '_ZN5ofLogC1EiRKSs', '_ZN5ofLogD1Ev',
                 '_ZN13ofSoundPlayer12setMultiPlayEb', '_ZN13ofSoundPlayer9setVolumeEf',
                 '_ZN13ofSoundPlayer4stopEv'):
        e.stub(name, lambda e: None)
    e.stub('_Z22ofGetElapsedTimeMillisv', lambda e: e.ret(0))

    def to_data_path(e):
        e.string_set(e.arg(0), e.string_get(e.arg(1)))
        e.ret(e.arg(0))
    e.stub('_Z12ofToDataPathRKSsb', to_data_path)

    def split(e):  # ofSplitString(source, delimiter) -> vector<string>
        parts = e.string_get(e.arg(1)).split(e.string_get(e.arg(2)))
        arr = e.alloc(8 * len(parts))
        for i, p in enumerate(parts):
            e.string_set(arr + 8 * i, p)
        end = arr + 8 * len(parts)
        e.put_u64(e.arg(0), arr)
        e.put_u64(e.arg(0) + 8, end)
        e.put_u64(e.arg(0) + 16, end)
        e.ret(e.arg(0))
    e.stub('_Z13ofSplitStringRKSsS0_', split)

    def to_int(e):  # istringstream >> int
        s = e.string_get(e.arg(0)).strip()
        n = 0
        while n < len(s) and (s[n:n + 1].isdigit() or (n == 0 and s[:1] in b'+-')):
            n += 1
        e.ret(int(s[:n]) if s[:n].lstrip(b'+-') else 0)
    e.stub('_Z7ofToIntRKSs', to_int)


class Random:
    """ofRandom stub that replays a list of choices and records the draws made."""

    def __init__(self, e):
        self.choices = []
        self.draws = []
        self.on_draw = None
        e.stub('_Z8ofRandomf', self._draw)

    def _draw(self, e):
        n = int(e.farg())
        k = self.choices[len(self.draws)] if len(self.draws) < len(self.choices) else 0
        self.draws.append(n)
        if self.on_draw:
            self.on_draw(n)
        e.ret_float(float(k))

    def reset(self, choices):
        self.choices = list(choices)
        self.draws = []


def explore(run, rnd):
    """Run `run()` for every combination of random outcomes. Yields (choices, result)."""
    stack = [()]
    while stack:
        choices = stack.pop()
        rnd.reset(choices)
        result = run()
        if len(rnd.draws) > len(choices):
            n = rnd.draws[len(choices)]
            for k in reversed(range(n)):
                stack.append(choices + (k,))
        else:
            yield choices, list(rnd.draws), result


# --- patterns -----------------------------------------------------------------------------------

def extract_patterns(e, rnd, markers):
    """Wave types of gameclass::generatewave. Walls keep the distances the game places them at.
    The delay before the next wave is measured at wall speed 1: generatewave scales it by
    1 / speed, so it is the distance the walls travel before the next wave."""
    game = e.alloc(C.GAMECLASS_SIZE)
    g = C.GAME
    marks = []
    rnd.on_draw = lambda n: marks.append(e.i32(game + g['nenemies']))

    def run(t):
        def go():
            e.write(game + g['enemies'], b'\0' * (C.ENEMY_SIZE * 500))
            e.put_i32(game + g['nenemies'], 0)
            e.put_f32(game + g['speed'], 1.0)
            e.put_f32(game + g['wavetimer'], -12345.0)
            e.put_f32(game + g['speedramp'], -12345.0)
            e.put_i32(game + g['pausewaves'], 0)
            del marks[:]
            e.call('_ZN9gameclass12generatewaveEi', game, t)
            walls = []
            for i in range(e.i32(game + g['nenemies'])):
                side, dist, length = struct.unpack('<iii', e.read(game + g['enemies'] + i * C.ENEMY_SIZE, 12))
                walls.append((side, dist, length))
            props = {}
            delay = e.f32(game + g['wavetimer'])
            if delay != -12345.0:
                props['delay_distance'] = int(delay) if delay == int(delay) else delay
            if e.i32(game + g['pausewaves']):
                props['hold_until_morphed'] = True
            ramp = e.f32(game + g['speedramp'])
            if ramp != -12345.0:
                props['speed_ramp'] = int(ramp) if ramp == int(ramp) else ramp
            return walls, list(marks), props
        return go

    patterns = OrderedDict()
    for t in range(0, 1200):
        rnd.reset([])
        walls, _, props = run(t)()
        if not walls and not props:
            continue
        runs = {choices: result for choices, draws, result in explore(run(t), rnd)}
        patterns[str(t)] = pattern_body(runs, markers)
    rnd.on_draw = None
    return patterns


def wall_json(w, markers):
    side, dist, length = w
    if side in markers:
        return {'event': markers[side], 'at': dist}
    return [side, dist, length]


def pattern_body(runs, markers):
    def node(prefix, start):
        sample = next(r for c, r in runs.items() if c[:len(prefix)] == prefix)
        walls, marks, props = sample
        if len(marks) == len(prefix):
            return dict(walls=[wall_json(w, markers) for w in walls[start:]], **props)
        n_at = marks[len(prefix)]
        pre = [wall_json(w, markers) for w in walls[start:n_at]]
        n = draw_size(prefix)
        children = [node(prefix + (k,), n_at) for k in range(n)]
        if n > 1 and all(shift(children[k], k, n) == children[0] for k in range(n)):
            body = {'walls': pre, 'rotate': n, 'then': children[0]}
        else:
            variants = []
            for c in children:
                if variants and variants[-1][1] == c:
                    variants[-1][0] += 1
                else:
                    variants.append([1, c])
            body = {'walls': pre, 'variants': variants}
        return body

    def draw_size(prefix):
        # every run through `prefix` makes the same next draw; its size is the number of branches
        return 1 + max(c[len(prefix)] for c in runs if c[:len(prefix)] == prefix and len(c) > len(prefix))

    body = node((), 0)
    return tidy(hoist(body))


def shift(body, k, n):
    """Express `body` (placed under rotation k) relative to rotation 0. Only the body's own walls
    move: a nested rotate draws a new rotation, and a variant draw resets it (the original reuses
    the rotation variable for the variant choice, and variant walls use absolute sides)."""
    out = dict(body)
    out['walls'] = [w if isinstance(w, dict) else [(w[0] - k) % n, w[1], w[2]] for w in body.get('walls', [])]
    return out


PROPS = ('delay_distance', 'hold_until_morphed', 'speed_ramp')


def leaves(body):
    if 'then' in body:
        return leaves(body['then'])
    if 'variants' in body:
        return [l for _, v in body['variants'] for l in leaves(v)]
    return [body]


def hoist(body):
    """Move properties shared by every outcome up to the pattern itself."""
    ls = leaves(body)
    for p in PROPS:
        vals = [l.get(p) for l in ls]
        if vals[0] is not None and all(v == vals[0] for v in vals):
            for l in ls:
                l.pop(p, None)
            body[p] = vals[0]
    return body


def tidy(body):
    out = OrderedDict()
    for k in ('walls', 'rotate', 'then', 'variants') + PROPS:
        if k not in body:
            continue
        v = body[k]
        if k == 'walls' and not v:
            continue
        if k == 'then':
            v = tidy(v)
        if k == 'variants':
            v = [[w, tidy(b)] for w, b in v]
        out[k] = v
    return out


# --- palettes -----------------------------------------------------------------------------------

def extract_palettes(e, ids):
    gfx = e.alloc(C.GRAPHICS_SIZE)

    def run(fill, pid):
        e.write(gfx + C.PALETTE_ARRAYS, struct.pack('<i', fill) * 60)
        e.call('_ZN13graphicsclass6setpalEib', gfx, pid, 0)
        return struct.unpack('<60i', e.read(gfx + C.PALETTE_ARRAYS, 240))

    fills = (0x11111111, 0x22222222)
    out = OrderedDict()
    for pid in sorted(ids):
        a, b = run(fills[0], pid), run(fills[1], pid)
        start, end = [], []
        for slot in range(10):
            cols = []
            for arr in range(6):
                i = arr * 10 + slot
                if a[i] == b[i]:
                    cols.append(a[i])
                elif (a[i], b[i]) == fills:
                    cols.append(None)
                else:
                    raise RuntimeError('palette %d slot %d depends on the previous palette' % (pid, slot))
            for half, dst in ((cols[:3], start), (cols[3:], end)):
                if all(c is None for c in half):
                    dst.append(None)
                elif any(c is None for c in half):
                    raise RuntimeError('palette %d slot %d is partly set' % (pid, slot))
                else:
                    dst.append(half)
        out[str(pid)] = OrderedDict(start=start, end=end)
    return out


def palette_ids(obj, start):
    """Palette ids the content can reach: literal ids, and ids computed from the current palette
    (evaluated over the reachable ones until nothing new turns up)."""
    found = set(start)
    exprs = []

    def walk(o):
        if isinstance(o, dict):
            for k, v in o.items():
                if k == 'palette' and isinstance(v, int):
                    found.add(v)
                elif k == 'palette' and isinstance(v, list):
                    exprs.append(v)
                else:
                    walk(v)
        elif isinstance(o, list):
            for v in o:
                walk(v)
    walk(obj)

    def ev(e, pal):
        if isinstance(e, int):
            return e
        if e == 'palette':
            return pal
        op, a, b = e[0], ev(e[1], pal), ev(e[2], pal)
        if op == '%':
            return a % b if a >= 0 else -((-a) % b)
        return {'+': a + b, '-': a - b, '*': a * b}[op]
    while True:
        new = {ev(e, p) for e in exprs for p in found} - found
        if not new:
            return found
        found |= new


# --- audio --------------------------------------------------------------------------------------

def extract_audio(e, rnd, game_dir, files, tracks, effects):
    """Music tracks (the numbers musicclass::play is called with) and sound effects (the numbers
    musicclass::playef is called with), as loaded by musicclass::loadmusic. Returns the audio
    description and a map from effect number to sound id."""
    music = e.alloc(C.MUSICCLASS_SIZE)
    m = C.MUSIC
    songs = music + m['songs']
    slot = lambda p: (p - songs) // C.SOUNDPLAYER_SIZE
    loads, loops = {}, {}
    e.stub('_ZN13ofSoundPlayer9loadSoundERKSsb',
           lambda e: loads.__setitem__(slot(e.arg(0)), e.string_get(e.arg(1)).decode()))
    e.stub('_ZN13ofSoundPlayer7setLoopEb', lambda e: loops.__setitem__(slot(e.arg(0)), bool(e.arg(1) & 0xff)))
    e.call('_ZN10musicclass9loadmusicEv', music)

    played = []
    positions = []
    e.stub('_ZN13ofSoundPlayer4playEv', lambda e: played.append(slot(e.arg(0))))
    e.stub('_ZN13ofSoundPlayer13setPositionMSEi', lambda e: positions.append(e.iarg(1)))
    e.stub('_ZN10musicclass12loadsongdataEi', lambda e: None)

    sounds = OrderedDict()
    effect_ids = {}
    for n in sorted(effects):
        del played[:]
        e.call('_ZN10musicclass6playefEi', music, n)
        if len(played) != 1:
            raise RuntimeError('playef(%d) played %r' % (n, played))
        path = loads[played[0]]
        sid = os.path.splitext(os.path.basename(path))[0]
        sounds[sid] = 'sounds/%s.ogg' % sid
        files[sounds[sid]] = os.path.join(game_dir, 'data', path)
        effect_ids[n] = sid

    # play(): which file, the track length, and where a repeated play starts
    out = OrderedDict()
    for song in sorted(tracks):
        def go():
            e.put_i32(music + m['cursong'], -1)
            e.put_i32(music + m['playcount'], 1)
            del positions[:]
            del played[:]
            e.call('_ZN10musicclass4playEi', music, song)
            return positions[-1] if positions else 0, tuple(played)
        weights = OrderedDict()
        slots = set()
        for _, _, (pos, p) in explore(go, rnd):
            weights[pos] = weights.get(pos, 0) + 1
            slots.add(p)
        if len(slots) != 1 or len(next(iter(slots))) != 1:
            raise RuntimeError('play(%d) played %r' % (song, slots))
        index = next(iter(slots))[0]
        tid = 'track%d' % song
        out[tid] = OrderedDict(
            file='music/%s.ogg' % tid,
            length_ms=e.i32(music + m['songlen_ms']),
            beats='music/%s.beats.json' % tid,
            restart_points_ms=[[w, p] for p, w in weights.items()],
            loop=loops.get(index, False),
        )
        files[out[tid]['file']] = os.path.join(game_dir, 'data', loads[index])
    return OrderedDict(music=out, sounds=sounds), effect_ids


def extract_beats(e, tracks):
    music = e.alloc(C.MUSICCLASS_SIZE)
    table = music + C.MUSIC['beattable']
    out = {}
    unset = -0x7777777
    for song in sorted(tracks):
        e.write(table, struct.pack('<i', unset) * C.BEATTABLE_LEN)
        e.call('_ZN10musicclass12loadsongdataEi', music, song)
        vals = struct.unpack('<%di' % C.BEATTABLE_LEN, e.read(table, 4 * C.BEATTABLE_LEN))
        n = max(i for i, v in enumerate(vals) if v != unset) + 1
        # entries the game leaves untouched keep the previous track's values; use 0
        out['track%d' % song] = [0 if v == unset else v for v in vals[:n]]
    return out


# --- font ---------------------------------------------------------------------------------------

class Found(Exception):
    pass


def extract_font(e):
    """The font file text is drawn with: ofTrueTypeFont opens its font files (FreeType's
    FT_New_Face) and then loads glyphs from them, starting with the printable characters. The file
    of the face the first printable glyph comes from is the text font."""
    faces = {}

    def new_face(e):  # FT_New_Face(library, path, index, &face)
        handle = 0x1000 * (len(faces) + 1)
        faces[handle] = e.cstr(e.arg(1)).decode()
        e.put_u64(e.arg(3), handle)
        e.ret(0)

    def char_index(e):  # FT_Get_Char_Index(face, charcode)
        raise Found(e.arg(0), e.arg(1) & 0xff)
    e.stub('FT_Init_FreeType', lambda e: e.ret(0))
    e.stub('FT_New_Face', new_face)
    e.stub('FT_Set_Char_Size', lambda e: e.ret(0))
    e.stub('FT_Get_Char_Index', char_index)
    font = e.alloc(0x1000)
    e.lenient = True
    try:
        e.call('_ZN14ofTrueTypeFontC1Ev', font)
        e.call('_ZN14ofTrueTypeFont4loadEib', font, 12, 0)
        raise RuntimeError('no glyphs loaded')
    except Found as f:
        face, char = f.args
    finally:
        e.lenient = False
    if not (0x20 < char < 0x7f) or face not in faces:
        raise RuntimeError('first glyph %r from %#x' % (char, face))
    return faces[face]


# --- text ---------------------------------------------------------------------------------------

def extract_text(e, levels, origin, n_ranks):
    """Level names and difficulty labels, the hyper badge, rank names, the completion messages and
    the credits.

    Names come from gameclass::stagename and getlevel, the testers from initcredits. The rest is
    found by drawing the GUI (gameclass::drawgui_text) and comparing what it prints:
    - the title screen, in every state (title page, menu cursor, arcade mode): the big lines it
      always prints are the game's title;
    - stage select, once per highlighted level: the string printed for only that level is its
      difficulty; the one printed for every hyper level and no other is the hyper badge. The
      colour the level's name is printed in is recovered by drawing it again with the GUI glow,
      the slow sine counter and the on-screen palette varied (see menu_colour);
    - game over, with no unlock, a level unlocked, a hyper level unlocked and the game completed:
      what all three unlocks print is the heading, what both level unlocks print is the level
      complete line, and what each prints alone is its message. The completed-level count is
      left as the placeholder {completed};
    - credits, once per page, with the ending locked and unlocked (only the page label, printed
      last, changes). Without labels: what every page prints is the title (big) and the thanks line, what both tester pages print
      is their heading, what only the last page prints is the rewatch button, and what only the
      first page prints is the main credits, as (role, name, site).
    Credit links are found by tapping each entry's row of the credits screen in the touch input
    handler (superhex::generickeypoll) and catching the URL it opens; the PC mouse path builds
    them but never opens them.
    """
    ret = e.new_string()
    ranks = []
    for i in range(n_ranks):
        e.call('_ZN9gameclass8getlevelEi', ret, 0, i)
        ranks.append(e.string_get(ret).decode('latin1'))

    game = e.alloc(C.GAMECLASS_SIZE)
    gfx = e.alloc(C.GRAPHICS_SIZE)
    helper = e.alloc(0x100)
    for base, members in ((game, C.GAME_STRINGS), (gfx, C.GRAPHICS_STRINGS)):
        for off, n in members:
            for i in range(n):
                e.string_init(base + off + 8 * i)
    e.call('_ZN9gameclass11initcreditsEv', game)
    begin, end = e.u64(game + C.GAME['credits']), e.u64(game + C.GAME['credits'] + 8)
    testers = [e.string_get(p).decode('latin1') for p in range(begin, end, 8)]

    printed = []
    colours = {}  # string -> (r, g, b) it was last printed in
    def on_print(e, big):
        s = e.string_get(e.arg(3)).decode('latin1')
        printed.append((s, big))
        colours[s] = (e.iarg(4), e.iarg(5), e.stack_arg(0))
    for fn in ('_ZN13graphicsclass5printEiiSsiiib', '_ZN13graphicsclass6rprintEiiSsiii',
               '_ZN13graphicsclass8bigprintEiiSsiiibi', '_ZN13graphicsclass9rbigprintEiiSsiiibi'):
        e.stub(fn, lambda e, big='big' in fn: on_print(e, big))
    for fn in ('_ZN13graphicsclass3lenESsi', '_ZN13graphicsclass9drawimageEiiib', '_Z10ofSetColoriii',
               '_Z16ofBigPictureModev', '_ZN9gameclass14drawleftbuttonER13graphicsclassiib',
               '_ZN9gameclass15drawrightbuttonER13graphicsclassiib', '_Z14superRemainderdi',
               '_ZN9gameclass19drawskewpoly_centerER13graphicsclassiiiii'):
        e.stub(fn, lambda e: e.ret(0))

    def empty(e):
        e.string_set(e.arg(0), b'')
        e.ret(e.arg(0))
    def count(e):
        e.string_set(e.arg(0), b'{completed}')
        e.ret(e.arg(0))
    e.stub('_Z10ofToStringIiESsRKT_', count)
    for fn in ('_Z10ofToStringISsESsRKT_', '_Z10ofToStringIcESsRKT_',
               '_ZN9gameclass11getbestmenuEiR9helpclass', '_ZN9gameclass14converttotimerEiR9helpclass',
               '_ZN9gameclass14getcontrolnameESs', '_ZN9gameclass7getbestER9helpclass', '_ZN9helpclass9twodigitsEi'):
        e.stub(fn, empty)

    def draw(stage, selection=0, won=1, menuscreen=0, menu=False, unlock=0, hyper=0, big=False, title=(0, 0, 0)):
        g = C.GAME
        for f, v in zip(('titlepage', 'menucursor', 'arcademode'), title):
            e.put_i32(game + g[f], v) if f != 'arcademode' else e.write(game + g[f], bytes([v]))
        e.put_i32(game + g['stage'], stage)
        e.put_i32(game + g['menuscreen'], menuscreen)
        e.put_i32(game + g['menuselection'], selection)
        e.put_i32(game + g['unlockevent'], unlock)
        e.write(game + g['hyper'], bytes([hyper]))
        for i in range(9):
            e.put_i32(game + g['won'] + 4 * i, won)
        # menus and the game-over screen are drawn once the game has zoomed out
        e.put_i32(game + g['zoom'], 320 if menu else 0)
        e.put_f32(game + g['gameovertimer'], 100.0 if menu else 0.0)
        del printed[:]
        e.call('_ZN9gameclass12drawgui_textER13graphicsclassR9helpclass', game, gfx, helper)
        return [(p, b) if big else p for p, b in printed if p]

    def one(found, what):
        found = set(found)
        if len(found) != 1:
            raise RuntimeError('expected one %s, found %r' % (what, found))
        return found.pop()

    menu = [l for l in levels.values() if 'menu_slot' in l]
    shown = {l['id']: set(draw(-2, l['menu_slot'])) for l in menu}
    hyper = {lid for lid in shown if origin[lid][1]}
    out = OrderedDict()
    for lid, strs in shown.items():
        others = set().union(*(s for k, s in shown.items() if k != lid))
        diff = strs - others
        if len(diff) != 1:
            raise RuntimeError('no unique difficulty label for %s: %r' % (lid, diff))
        slot = levels[lid]['menu_slot']
        # the name is the stage select's big text (hyper levels show their normal level's name,
        # with the badge)
        name = one([s for s, big in draw(-2, slot, big=True) if big], 'stage select name')
        out[lid] = OrderedDict(name=name, difficulty=diff.pop(),
                               menu_colour=menu_colour(e, lambda: draw(-2, slot, big=True), colours, gfx, helper))
        out[lid].update(menu_panels(e, game, gfx, helper, slot, colours, lambda: draw(-2, slot)))
    badges = set.intersection(*(shown[h] for h in hyper)) - set().union(*(shown[k] for k in shown if k not in hyper))
    if len(badges) != 1:
        raise RuntimeError('no unique hyper badge: %r' % badges)
    badge = badges.pop()

    states = [draw(-1, menu=True, big=True, title=(p, c, a)) for p in range(3) for c in range(3) for a in range(2)]
    title_lines = [s.strip() for s, big in states[0] if big and all((s, big) in st for st in states)]
    if not title_lines:
        raise RuntimeError('no title found on the title screen')

    over = draw(0, menu=True)
    level, hyper_level, game_done = (draw(0, menu=True, unlock=u, hyper=h) for u, h in ((1, 0), (1, 1), (2, 0)))
    heading = one(set(level) & set(hyper_level) & set(game_done) - set(over), 'completion heading')
    complete = one(set(level) & set(hyper_level) - set(game_done) - set(over), 'level complete line')
    completion = OrderedDict(heading=heading, level_complete=complete)
    for key, mine, rest in (('new_hyper', level, (hyper_level, game_done)),
                            ('sides_complete', hyper_level, (level, game_done)),
                            ('game_complete', game_done, (level, hyper_level))):
        completion[key] = one(set(mine) - set().union(over, *rest), key)

    pages = [draw(-1, p, menuscreen=1, menu=True, big=True) for p in range(4)]
    locked = [draw(-1, p, won=0, menuscreen=1, menu=True, big=True) for p in range(3)]
    # the page label is what changes with the ending unlocked; it is always printed last
    for p, q in zip(pages, locked):
        if set(p) ^ set(q) != {p[-1], q[-1]}:
            raise RuntimeError('credits page label is not the last line printed: %r / %r' % (p, q))
    pages = [p[:-1] for p in pages]
    every = set.intersection(*map(set, pages))
    title = one([s for s, big in every if big], 'credits title')
    thanks = one([s for s, big in every if not big], 'credits thanks line')
    testers_heading = one(set(pages[1]) & set(pages[2]) - set(pages[0]) - set(pages[3]), 'testers heading')[0]
    rewatch = one(set(pages[3]) - set().union(*map(set, pages[:3])), 'rewatch button')[0]
    others = set().union(*map(set, pages[1:]))
    main = [s for s, _ in pages[0] if (s, _) not in others]
    if len(main) % 3:
        raise RuntimeError('main credits are not (role, name, site) triples: %r' % main)
    entries = [OrderedDict(zip(('role', 'name', 'site'), main[i:i + 3])) for i in range(0, len(main), 3)]
    for entry, url in zip(entries, credit_links(e, len(entries))):
        if url:
            entry['url'] = url
    credits = OrderedDict(title=title, thanks=thanks, main=entries, testers_heading=testers_heading,
                          testers=testers, rewatch_ending=rewatch)
    return out, badge, ranks, title_lines, OrderedDict(completion=completion, credits=credits)


def menu_panels(e, game, gfx, helper, slot, colours, draw_text):
    """How a level's panels look: whether the stage select draws its panel with a border
    (gameclass::drawgui_shapes, with the panel functions stubbed), and the colour of text on its
    buttons (the start prompt, printed by drawgui_text)."""
    G = C.GAME
    bordered = []
    e.stub('_ZN9gameclass26drawskewpoly_center_borderER13graphicsclassiiiii', lambda e: bordered.append(True))
    for fn in ('_ZN9gameclass13drawgui3dquadER13graphicsclassddddddddi', '_ZN9gameclass11drawguipolyER13graphicsclassiiiiiiii',
               '_ZN9gameclass26drawskewpoly_center_buttonER13graphicsclassiiiii',
               '_ZN9gameclass17drawskewpoly_leftER13graphicsclassiiii', '_ZN9gameclass8drawlineER13graphicsclassiiiii'):
        e.stub(fn, lambda e: None)
    e.put_i32(game + G['stage'], -2)
    e.put_i32(game + G['menuselection'], slot)
    e.call('_ZN9gameclass14drawgui_shapesER13graphicsclassR9helpclass', game, gfx, helper)

    prompt = game + G['prompts'] + 8 * 1  # prompts[1], the start prompt
    e.string_set(prompt, b'START PROMPT')
    draw_text()
    e.string_set(prompt, b'')
    return OrderedDict(panel_border=bool(bordered), button_text=list(colours['START PROMPT']))


def menu_colour(e, draw, colours, gfx, helper):
    """The colour a level's name is printed in on the stage select (its only big text), as a pack
    colour.

    Each channel is drawn at two glow values, which gives it as base + factor * glow; if it
    changes with the on-screen palette instead, the colour is that palette slot. The slow sine
    counter, which steps through 0..63 once per tick, is set to the start of each quarter of
    its range; if the colour changes with it, it cycles every 16 ticks."""
    G, H = C.GRAPHICS, C.HELP

    def sample(glow, slowsine, palbase):
        e.put_f32(helper + H['glow'], glow)
        e.put_f32(helper + H['slowsine'], slowsine)
        for c, key in enumerate(('curpal_r', 'curpal_g', 'curpal_b')):
            for k in range(10):
                e.put_i32(gfx + G[key] + 4 * k, palbase + 10 * k + c)
        big = [s for s, b in draw() if b]
        if len(big) != 1:
            raise RuntimeError('expected one big string on the stage select, got %r' % big)
        return colours[big[0]]

    def colour(slowsine):
        a, b, p = sample(0.0, slowsine, 0), sample(40.0, slowsine, 0), sample(0.0, slowsine, 100)
        if a != p:
            # palette: every channel must name the same slot
            slots = {(v - 10 * 0 - c) // 10 for c, v in enumerate(a)}
            if a == b and len(slots) == 1 and all(p[c] - a[c] == 100 for c in range(3)):
                return OrderedDict(slot=slots.pop())
            raise RuntimeError('unrecognised palette colour %r / %r' % (a, p))
        out = []
        for c in range(3):
            k = (b[c] - a[c]) / 40.0
            out.append(a[c] if k == 0 else [a[c], int(k) if k == int(k) else k])
        return out

    frames = [colour(16.0 * q) for q in range(4)]
    if all(f == frames[0] for f in frames):
        return frames[0]
    return OrderedDict(cycle=frames, ticks=16)


def credit_links(e, n):
    """URL opened by tapping each of the n main credit entries (None if it has no link). On the
    credits screen the entries fill rows ui[58]..ui[59], ui[59]..ui[60], and so on, of equal height."""
    S, G, GR = C.SUPERHEX, C.GAME, C.GRAPHICS
    app = e.alloc(C.SUPERHEX_SIZE)
    game, gfx = app + S['game'], app + S['graphics']
    for base, members in ((game, C.GAME_STRINGS), (gfx, C.GRAPHICS_STRINGS)):
        for off, k in members:
            for i in range(k):
                e.string_init(base + off + 8 * i)
    e.put_i32(game + G['inputtype'], 1)  # touch
    e.put_i32(game + G['touchlayout'], 3)  # credits screen
    e.put_i32(game + G['menuselection'], 0)  # first page
    e.put_i32(gfx + GR['screenw'], 900)
    row = 100
    for k in range(3):
        e.put_i32(gfx + GR['ui'] + 4 * (58 + k), row * (k + 1))
    opened = []
    e.stub('_ZN9gameclass7gotourlESs', lambda e: opened.append(e.string_get(e.arg(1)).decode()))
    links = []
    for i in range(n):
        e.write(app + S['mouseclicked'], b'\x01')
        e.put_i32(app + S['touchx'], 450)
        e.put_i32(app + S['touchy'], row * (i + 1) + row // 2)
        del opened[:]
        e.call('_ZN8superhex14generickeypollEv', app)
        if len(opened) > 1:
            raise RuntimeError('one tap opened %r' % opened)
        links.append(opened[0] if opened else None)
    return links


# --- pack ---------------------------------------------------------------------------------------

def dump(obj):
    return json.dumps(obj, indent=1, ensure_ascii=False).encode('utf-8')


def main():
    if len(sys.argv) != 3:
        sys.exit('usage: extract.py GAME_DIR OUT.zip')
    game_dir, out_path = sys.argv[1], sys.argv[2]
    binary = os.path.join(game_dir, 'SuperHexagon')
    with open(binary, 'rb') as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    if digest != BINARY_SHA256:
        sys.exit('unsupported SuperHexagon binary (sha256 %s); expected Linux build 8838351' % digest)

    e = Emu(binary)
    setup(e)
    rnd = Random(e)
    files = OrderedDict()

    print('levels...')
    setups, switch_levels, origin, setup_sounds = LV.extract_levels(e, rnd)
    times, rank_events, event_sounds, palette_roles, marker_fields = LV.roles(e, rnd)
    markers = OrderedDict()
    for side, (field, value) in marker_fields.items():
        (action,) = logic.write([], field, value, False)  # named as the logic names them
        markers[side] = action['morph'] if 'morph' in action else next(iter(action))
    print('patterns...')
    patterns = extract_patterns(e, rnd, markers)
    print('  %d patterns' % len(patterns))
    print('text...')
    level_text, badge, rank_names, title, text = extract_text(e, setups, origin, len(times) + 1)
    pulses = LV.pulse_rules(e, rnd, origin)
    print('logic...')
    lifted, on_hit, tutorial = logic.lift_all(binary)

    def empty_wave(node):  # a wave type generatewave makes nothing of: a no-op
        return isinstance(node, dict) and 'pattern' in node and node['pattern'] not in patterns

    def resolve(node):  # the 3-minute switch's target level and clock change (levels.py)
        if empty_wave(node):
            return []
        if isinstance(node, dict):
            if 'switch_level' in node:
                return {'switch_level': switch_levels[node['switch_level']['changetostage']]}
            return OrderedDict((k, resolve(v)) for k, v in node.items())
        if isinstance(node, (list, tuple)):
            if len(node) == 2 and isinstance(node[0], int):  # a weighted pick entry
                return type(node)(resolve(v) for v in node)
            return type(node)(resolve(v) for v in node if not empty_wave(v))
        return node
    lifted = {k: resolve(v) for k, v in lifted.items()}
    levels = []
    for lid, level_setup in setups.items():
        on_tick, on_wave = lifted[level_setup['director']]
        lvl = OrderedDict(id=lid)
        if lid in level_text:
            lvl.update(level_text[lid])
            if origin[lid][1]:
                lvl['badge'] = badge
        lvl.update((k, v) for k, v in level_setup.items() if k not in ('id', 'counters'))
        lvl['beat_divisor'], frozen = pulses[lid]
        if frozen is not None:
            lvl['frozen_pulse'] = frozen
        used = logic.counters(on_tick, on_wave)
        lvl['counters'] = OrderedDict((c, level_setup['counters'].get(c, 0)) for c in used)
        if lvl.get('kind') == 'ending':
            lvl['on_tick'] = on_tick
        else:
            camera, entries = timeline.run(on_tick, lvl['time_offset'], lvl['palette'], lvl['counters'])
            lvl['camera'] = camera
            lvl['timeline'] = [OrderedDict(at=t, do=acts) for t, acts in entries]
        levels.append(lvl)
    finale = OrderedDict(on_hit=on_hit, on_tick=lifted['finale'][0], on_wave=lifted['finale'][1])
    tutorial = OrderedDict(counters=OrderedDict((c, 0) for c in logic.counters(*tutorial.values())), **tutorial)
    directors = OrderedDict(directors=OrderedDict(
        (name, OrderedDict(on_wave=wave)) for name, (_, wave) in lifted.items() if name != 'finale'))
    levels = OrderedDict(levels=levels, tutorial=tutorial, finale=finale)

    # audio: the tracks the levels play, and the sounds of ranks and engine events
    played = set()

    def music(node):
        if isinstance(node, dict):
            for k, v in node.items():
                if k == 'music' and isinstance(v, str):
                    played.add(int(v[len('track'):]))
                music(v)
        elif isinstance(node, list):
            for v in node:
                music(v)
    music([levels, directors])
    common = set.intersection(*(set(s) for s, _ in rank_events))
    if len(common) != 1:
        raise RuntimeError('no common rank-up sound: %r' % rank_events)
    sound_roles = OrderedDict(rank_up=list(common))
    sound_roles.update(setup_sounds)
    sound_roles.update(event_sounds)
    effects = {n for v in sound_roles.values() for n in v} | {n for s, _ in rank_events for n in s}
    print('audio...')
    beats = extract_beats(e, played)  # before extract_audio stubs out loadsongdata
    audio, effect_ids = extract_audio(e, rnd, game_dir, files, played, effects)
    audio['roles'] = OrderedDict((k, [effect_ids[n] for n in v] if len(v) != 1 else effect_ids[v[0]])
                                 for k, v in sound_roles.items())
    print('font...')
    font = extract_font(e)
    files[font] = os.path.join(game_dir, 'data', font)

    print('rotation modes...')
    used = set()

    def rotations(node):
        if isinstance(node, dict):
            for k, v in node.items():
                if k in ('rotation', 'reroll_rotation', 'random_rotation'):
                    used.update(v if isinstance(v, list) else [v])
                else:
                    rotations(v)
        elif isinstance(node, list):
            for v in node:
                rotations(v)
    rotations([levels, directors])
    modes = LV.rotation_modes(e, rnd, used)

    print('palettes...')
    ids = palette_ids([levels, directors], [l['palette'] for l in levels['levels']]) | set(palette_roles.values())
    palettes = OrderedDict(palettes=extract_palettes(e, ids), roles=palette_roles)

    ranks = [OrderedDict(name=rank_names[0], at=0)]
    for k, (t, (sounds, completes)) in enumerate(zip(times, rank_events)):
        r = OrderedDict(name=rank_names[k + 1], at=t)
        voice = [n for n in sounds if n not in common]
        if len(voice) > 1:
            raise RuntimeError('rank %d plays %r' % (k + 1, sounds))
        if voice:
            r['voice'] = effect_ids[voice[0]]
        if completes:
            r['completes_level'] = True
        ranks.append(r)
    manifest = OrderedDict(
        format=FORMAT,
        id='superhexagon-original',
        name=' '.join(title),
        title=title,
        version='1.0.0',
        description='Extracted from Super Hexagon (Linux build 8838351)',
        files=OrderedDict(levels='levels.json', directors='directors.json', patterns='patterns.json',
                          palettes='palettes.json', audio='audio.json', text='text.json'),
        font=font,
        ranks=ranks,
        rotation_modes=modes,
    )

    with zipfile.ZipFile(out_path, 'w') as z:
        def put(name, data, compress=True):
            z.writestr(zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0)), data,
                       compress_type=zipfile.ZIP_DEFLATED if compress else zipfile.ZIP_STORED)
        put('pack.json', dump(manifest))
        put('levels.json', dump(levels))
        put('directors.json', dump(directors))
        put('patterns.json', dump(OrderedDict(patterns=patterns)))
        put('palettes.json', dump(palettes))
        put('audio.json', dump(audio))
        put('text.json', dump(text))
        for tid, b in beats.items():
            put('music/%s.beats.json' % tid, json.dumps(b, separators=(',', ':')).encode())
        for name, src in files.items():
            with open(src, 'rb') as f:
                put(name, f.read(), compress=False)
    print('wrote %s' % out_path)


if __name__ == '__main__':
    main()
