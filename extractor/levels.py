"""Level setups, recovered by running the original's own code (see emu.py).

- stage select (superhex::gameinput): which stage and hyper flag each menu slot starts, and which
  completion flag unlocks it;
- gameclass::winlevel: the completion flag each level sets, and what completing it leads to;
- the run start (superhex::gameinput, select after a game over): music, palette, turn speed,
  centre flip, starting rotations, start wave, speed and counters;
- gameclass::secretending: the same for the ending;
- gameclass::changetostage: the 3-minute switch, matched against the run starts to find the level
  it switches to;
- superhex::gamelogic: the clock the level logic sees, which gives the time offsets; and how the
  centre's pulse follows the music's beat values.
"""
import struct
from collections import OrderedDict

import capstone

import content as C

FORMATS = {'i': '<i', 'f': '<f', 'd': '<d', 'b': '<B', 'l': '<q'}


class Machine:
    """A superhex object in the emulator, with the callees of the functions run on it recorded."""

    def __init__(self, e, rnd):
        self.e = e
        self.rnd = rnd
        self.app = e.alloc(C.SUPERHEX_SIZE)
        self.log = []
        self.fields = dict(C.SUPERHEX_FIELDS, **C.SETUP_FIELDS)
        self.md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
        self.names = {a: n for n, a in e.symbols.items()}
        self.hooked = set()
        self.saved = {}  # name -> the stub it had before (None: it ran its own code)

    def reset(self, **values):
        e = self.e
        e.write(self.app, b'\0' * C.SUPERHEX_SIZE)
        for base, members in ((self.app + 0x18, C.GAME_STRINGS), (self.app + 0xbfc8, C.GRAPHICS_STRINGS)):
            for off, n in members:
                for i in range(n):
                    e.string_init(base + off + 8 * i)
        for k, v in values.items():
            self[k] = v
        del self.log[:]

    def __setitem__(self, name, v):
        if isinstance(v, (list, tuple)):
            for i, x in enumerate(v):
                self.e.write(self.app + self.fields[name][0] + 4 * i, struct.pack('<i', x))
            return
        off, k = self.fields[name]
        self.e.write(self.app + off, struct.pack(FORMATS[k], v))

    def __getitem__(self, name):
        off, k = self.fields[name]
        fmt = FORMATS[k]
        return struct.unpack(fmt, self.e.read(self.app + off, struct.calcsize(fmt)))[0]

    def won(self):
        off = self.fields['won'][0]
        return list(struct.unpack('<9i', self.e.read(self.app + off, 36)))

    def stub_callees(self, fn, hooks=None):
        """Let `fn` run, and replace the functions it calls by recorders, except those that set up
        state (RUN)."""
        hooks = hooks or {}
        self._unstub(fn)
        self.hooked.discard(fn)
        addr = self.e.sym(fn)
        size = self.e.sizes[fn]
        for insn in self.md.disasm(self.e.read(addr, size), addr):
            if insn.mnemonic not in ('call', 'jmp') or not insn.op_str.startswith('0x'):
                continue
            target = int(insn.op_str, 16)
            if addr <= target < addr + size:
                continue  # a jump within the function
            name = self.names.get(target)  # a call, or a tail call
            if name in hooks:
                self._stub(name, hooks[name])
                self.hooked.add(name)
            elif name is not None and name not in RUN and name not in self.hooked:
                # imports are handled by the emulator; RUN functions execute
                self.hooked.add(name)
                self._stub(name, self._recorder(name))

    def _remember(self, name):
        if name not in self.saved:
            self.saved[name] = self.e.func_stubs.get(self.e.sym(name))

    def _stub(self, name, fn):
        self._remember(name)
        self.e.stub(name, fn)

    def _unstub(self, name):
        self._remember(name)
        self.e.unstub(name)

    def release(self):
        """Put back the stubs as they were before this machine changed them."""
        for name, previous in self.saved.items():
            if previous is None:
                self.e.unstub(name)
            else:
                self.e.stub(name, previous)
        self.saved.clear()
        self.hooked.clear()

    def _recorder(self, name):
        def rec(e):
            self.log.append((name, e.iarg(1)))
            e.ret(0)
        return rec

    def calls(self, name):
        return [a for n, a in self.log if n == name]


PLAY = '_ZN10musicclass4playEi'
SETPAL = '_ZN13graphicsclass6setpalEib'
CHANGEPAL = '_ZN13graphicsclass9changepalEi'
NEVER = -12345
RUN = {'_ZN9gameclass7restartEv', '_ZN9gameclass12restarthyperEv', '_ZN9gameclass10resetwavesEv',
       '_ZN9gameclass13changetostageEiR13graphicsclassR10musicclass', '_Z8ofRandomf'}


def setup_fields(m, reroll):
    """The level setup the code just left in the object (NEVER marks fields it didn't touch)."""
    pal = m.calls(SETPAL) + m.calls(CHANGEPAL)
    music = m.calls(PLAY)
    if len(pal) != 1 or len(music) != 1:
        raise RuntimeError('expected one palette and one track, got %r' % m.log)
    out = OrderedDict(music='track%d' % music[0], palette=pal[0], turn_speed=m['turnspeed'],
                      centre_flip=bool(m['centerflip']), rotation=reroll)
    start = OrderedDict(wave=m['wavecount'])
    if m['speed'] != NEVER:
        start['speed'] = int(m['speed']) if m['speed'] == int(m['speed']) else m['speed']
    out['start'] = start
    out['_sounds'] = m.calls('_ZN10musicclass6playefEi')
    counters = OrderedDict()
    for field, counter in (('hyperopening', 'hyper_opening'), ('shapewavecounter', 'shape'),
                           ('timelinestate', 'step'), ('timelinedelay', 'step_delay')):
        v = m[field]
        if v != NEVER:
            counters[counter] = int(v)
    return out, counters


def explore_rotations(m, run):
    """Run `run` over every random outcome; the rotation modes it can leave."""
    from extract import explore
    seen = []
    result = None
    for _, _, r in explore(run, m.rnd):
        if m['rotmode'] not in seen:
            seen.append(m['rotmode'])
        if result is not None and r != result:
            raise RuntimeError('setup depends on random draws other than the rotation')
        result = r
    return sorted(seen), result


def blank_setup(m, **values):
    m.reset(rotmode=-1, pal=-1, speed=float(NEVER), hyperopening=NEVER, shapewavecounter=float(NEVER),
            timelinestate=NEVER, timelinedelay=float(NEVER), wavecount=NEVER, **values)


def run_starts(m, stages):
    """Setup of a run for each (stage, hyper): select on the game-over screen."""
    def press(e):
        m['in_select'] = 1
    m.stub_callees('_ZN8superhex9gameinputEv', hooks={'_ZN8superhex14generickeypollEv': press})
    out = {}
    for stage, hyper in stages:
        def go():
            blank_setup(m, stage=stage, hyper=hyper, zoom=320, gameovertimer=100.0, tutorialflag=1)
            m.e.call('_ZN8superhex9gameinputEv', m.app)
            return repr(setup_fields(m, None))
        rotations, _ = explore_rotations(m, go)
        go()
        out[stage, hyper] = setup_fields(m, rotations)
    return out


def ending_start(m):
    m.stub_callees('_ZN9gameclass12secretendingER13graphicsclassR10musicclass')

    def go():
        blank_setup(m)
        m.e.call('_ZN9gameclass12secretendingER13graphicsclassR10musicclass', m.app + 0x18, m.app + 0xbfc8,
                 m.app + 0x1e4a8)
        return repr(setup_fields(m, None))
    rotations, _ = explore_rotations(m, go)
    go()
    return setup_fields(m, rotations)


def stage_select(m):
    """For each menu slot: (stage, hyper), and the completion flag that unlocks it (None if open)."""
    def press(e):
        m['in_select'] = 1
    m.stub_callees('_ZN8superhex9gameinputEv', hooks={'_ZN8superhex14generickeypollEv': press})

    def select(slot, won):
        m.reset(stage=-2, menuselection=slot, won=won, menumovecooldown=1.0, rotmode=-1)
        m['stage'] = -2
        m.e.call('_ZN8superhex9gameinputEv', m.app)
        return None if m['stage'] == -2 else (m['stage'], m['hyper'])
    out = {}
    for slot in range(16):
        picked = select(slot, [1] * 9)
        if picked is None:
            continue
        if select(slot, [0] * 9) is not None:
            out[slot] = (picked, None)
            continue
        flags = [i for i in range(9) if select(slot, [int(j == i) for j in range(9)]) is not None]
        if len(flags) != 1:
            raise RuntimeError('slot %d is unlocked by %r' % (slot, flags))
        out[slot] = (picked, flags[0])
    return out


def completions(m, stages):
    """For each (stage, hyper): the completion flag winlevel sets, its unlock event and postwin."""
    m.stub_callees('_ZN9gameclass8winlevelEv')
    out = {}
    for stage, hyper in stages:
        m.reset(stage=stage, hyper=hyper)
        m.e.call('_ZN9gameclass8winlevelEv', m.app + 0x18)
        flags = [i for i, w in enumerate(m.won()) if w]
        if len(flags) != 1:
            raise RuntimeError('winlevel set %r' % flags)
        out[stage, hyper] = (flags[0], m['unlockevent'], m['postwin'])
    return out


def switches(m):
    """changetostage(t) setups."""
    m.stub_callees('_ZN9gameclass13changetostageEiR13graphicsclassR10musicclass')
    out = {}
    for t in (0, 1):
        def go():
            blank_setup(m)
            m.e.call('_ZN9gameclass13changetostageEiR13graphicsclassR10musicclass', m.app + 0x18, t,
                     m.app + 0xbfc8, m.app + 0x1e4a8)
            return repr(setup_fields(m, None))
        rotations, _ = explore_rotations(m, go)
        go()
        out[t] = setup_fields(m, rotations)
    return out


def logic_clock(m, stage, hyper, switch=None, t=100):
    """Run gamelogic one tick, alive at survival time t, and return (stage, hyper, time) as the
    level logic sees them (read when it places a wave)."""
    seen = []

    def wave(e):
        seen.append((m['stage'], m['hyper'], m['time']))
    m.stub_callees('_ZN8superhex9gamelogicEv', hooks={'_ZN9gameclass12generatewaveEi': wave})
    m.reset(stage=stage, hyper=hyper, time=float(t), zoom=40, nsides=6, cursong=-1, expectedframedelta=1.0, rank=99,
            override_hexagoner=-1, override_hexagonest=-1)
    for i in range(6):
        m.e.put_i32(m.app + m.fields['won'][0] + 4 * i, 1)
    if switch is not None:
        m.e.call('_ZN9gameclass13changetostageEiR13graphicsclassR10musicclass', m.app + 0x18, switch,
                 m.app + 0xbfc8, m.app + 0x1e4a8)
        m['time'] = float(t)
    m['wavetimer'] = 0.0
    m.e.call('_ZN8superhex9gamelogicEv', m.app)
    if not seen:
        raise RuntimeError('no wave placed')
    return seen[0]


ANNOUNCE = {  # winlevel's unlock event -> what the game-over screen announces (drawgui_text)
    (1, 0): {'announce': 'new_hyper'},
    (1, 1): {'announce': 'sides_complete'},
    (2, 0): {'announce': 'game_complete', 'finale': True},
    (3, 1): {'announce': 'congratulations', 'ending': True},
}


def extract_levels(e, rnd):
    """Level setups, by level id; the 3-minute switches, by changetostage argument; and each
    level's (stage, hyper) in the original."""
    m = Machine(e, rnd)
    try:
        return _extract_levels(e, m)
    finally:
        m.release()


def _extract_levels(e, m):
    slots = stage_select(m)
    ret = e.new_string()
    ids = {}
    for (stage, hyper), _ in slots.values():
        e.call('_ZN9gameclass9stagenameEi', ret, 0, stage + 3 * hyper)  # the original's numbering
        ids[stage, hyper] = e.string_get(ret).decode('latin1').lower().replace(' ', '_')
    starts = run_starts(m, sorted(ids))
    done = completions(m, sorted(ids))
    flag_level = {flag: ids[sh] for sh, (flag, _, _) in done.items()}
    levels = OrderedDict()
    origin = {}
    for stage_hyper in sorted(ids, key=lambda sh: (sh[1], sh[0])):
        stage, hyper = stage_hyper
        lid = ids[stage_hyper]
        slot = [s for s, (sh, _) in slots.items() if sh == stage_hyper]
        if len(slot) != 1:
            raise RuntimeError('level %s is in menu slots %r' % (lid, slot))
        flag = slots[slot[0]][1]
        setup, counters = starts[stage_hyper]
        _, event, postwin = done[stage_hyper]
        completion = dict(ANNOUNCE[event, hyper])
        if completion.get('finale') and postwin != 1:
            raise RuntimeError('completing %s does not start the finale' % lid)
        _, _, seen = logic_clock(m, stage, hyper)
        lvl = OrderedDict(id=lid, menu_slot=slot[0], unlock=None if flag is None else {'completed': flag_level[flag]})
        lvl.update(setup)
        lvl['time_offset'] = int(seen - 101)
        lvl['director'] = ids[stage, 0]
        lvl['completion'] = completion
        lvl['counters'] = counters
        levels[lid] = lvl
        origin[lid] = stage_hyper

    sounds = OrderedDict()
    starting = {tuple(st.pop('_sounds')) for st, _ in starts.values()}
    if len(starting) != 1:
        raise RuntimeError('levels start with different sounds: %r' % starting)
    sounds['level_start'] = list(starting.pop())
    for lvl in levels.values():
        lvl.pop('_sounds')

    # the 3-minute switch: which level's setup changetostage(t) applies, and the clock change
    switch = {}
    switch_sounds = set()
    for t, (setup, counters) in switches(m).items():
        switch_sounds.add(tuple(setup.pop('_sounds')))
        match = [ids[sh] for sh, (st, co) in starts.items() if st == setup and co == counters]
        if len(match) != 1:
            raise RuntimeError('changetostage(%d) matches %r' % (t, match))
        switch[t] = match[0]
    switch_levels = {}
    for t, target in switch.items():
        # the clock after the switch, relative to the clock the level was running on, from each level
        offsets = set()
        for stage, hyper in ids:
            s, h, seen = logic_clock(m, stage, hyper, switch=t)
            if ids.get((s, h)) != target:
                raise RuntimeError('after changetostage(%d) the logic runs as %r' % (t, (s, h)))
            offsets.add(seen - logic_clock(m, stage, hyper)[2])
        if len(offsets) != 1:
            raise RuntimeError('changetostage(%d) changes the clock by %r' % (t, offsets))
        switch_levels[t] = {'level': target, 'time_offset': int(offsets.pop())}

    if len(switch_sounds) != 1:
        raise RuntimeError('level switches play different sounds: %r' % switch_sounds)
    sounds['switch_level'] = list(switch_sounds.pop())

    setup, counters = ending_start(m)
    sounds['ending_start'] = setup.pop('_sounds')
    ending = OrderedDict(id='ending', kind='ending')
    ending.update(setup)
    ending['time_offset'] = 0
    ending['director'] = 'ending'
    ending['counters'] = counters
    levels['ending'] = ending
    return levels, switch_levels, origin, sounds


# --- tuning, sound and palette roles -------------------------------------------------------------

PLAYEF = '_ZN10musicclass6playefEi'


def rank_times(e):
    """gameclass::timetable, set up by the constructor. The constructor goes on to load saves and
    talk to Steam, which isn't available here: that part is let fail."""
    from emu import EmuError
    game = e.alloc(C.GAMECLASS_SIZE)
    for off, n in C.GAME_STRINGS:
        for i in range(n):
            e.string_init(game + off + 8 * i)
    e.lenient = True
    try:
        e.call('_ZN9gameclassC1Ev', game)
    except EmuError:
        pass
    finally:
        e.lenient = False
    times = list(struct.unpack('<6i', e.read(game + C.GAME['timetable'], 24)))
    if not all(0 < a < b for a, b in zip(times, times[1:])):
        raise RuntimeError('rank times not set up: %r' % times)
    return times


def alive(m, **values):
    """A run in progress on stage 0, with no wave due."""
    m.reset(**dict(dict(stage=0, zoom=40, nsides=6, cursong=-1, expectedframedelta=1.0, rank=99, wavetimer=100.0,
                        override_hexagoner=-1, override_hexagonest=-1, speed=20.0), **values))


def tick(m, best=10 ** 6):
    m.stub_callees('_ZN8superhex9gamelogicEv', hooks={'_ZN9gameclass17getbesttime_stageEv': lambda e: e.ret(best)})
    m.e.call('_ZN8superhex9gamelogicEv', m.app)
    return m.log


def ranks(m, times):
    """Per rank: the sounds reaching it plays, and whether reaching it completes the level."""
    out = []
    for k, t in enumerate(times):
        alive(m, rank=k, time=float(t), levelreached=99)
        log = tick(m)
        out.append((m.calls(PLAYEF), any(n == '_ZN9gameclass8winlevelEv' for n, _ in log)))
    return out


def pulse_rules(e, rnd, origin):
    """Per level id: how its centre follows the beat (superhex::gamelogic). The pulse is kicked
    to |beat| / divisor, rounded down; some levels hold it at a fixed size while walls are frozen.
    Returns (divisor, frozen pulse or None)."""
    m = Machine(e, rnd)
    try:
        starts = {lid: (lambda s=s: alive(m, stage=s[0], hyper=s[1])) for lid, s in origin.items()}

        def ending():
            blank_setup(m)
            m.stub_callees('_ZN9gameclass12secretendingER13graphicsclassR10musicclass')
            m.e.call('_ZN9gameclass12secretendingER13graphicsclassR10musicclass', m.app + 0x18, m.app + 0xbfc8,
                     m.app + 0x1e4a8)
        starts['ending'] = ending
        return {lid: pulse_rule(m, start) for lid, start in starts.items()}
    finally:
        m.release()


def pulse_rule(m, start):
    music = m.app + 0x1e4a8

    def target(beat, freeze):
        start()
        m['cursong'] = 1
        m['sidechangefreeze'] = freeze
        m['pulse'] = 0.0
        m.e.write(music + C.MUSIC['beattable'], struct.pack('<i', beat) * C.BEATTABLE_LEN)
        m.e.write(music + C.MUSIC['songlen_ms'], struct.pack('<i', 10 ** 6))
        tick(m)
        return m['pulse'] + 1  # kicked to the target, then decayed once in the same tick

    beat = 120
    t = target(beat, 0.0)
    div = beat / t
    if div != int(div):
        raise RuntimeError('beat %d gives pulse %r' % (beat, t))
    div = int(div)
    # rounded down, and by magnitude
    if target(beat + div - 1, 0.0) != t or target(-beat, 0.0) != t:
        raise RuntimeError('pulse is not |beat| / %d rounded down' % div)
    frozen = target(beat, 50.0)
    return div, None if frozen == t else int(frozen)


def press(m, field):
    def poll(e):
        m[field] = 1
    m.stub_callees('_ZN8superhex9gameinputEv', hooks={'_ZN8superhex14generickeypollEv': poll})
    m.e.call('_ZN8superhex9gameinputEv', m.app)
    return m.log


def sound_events(m):
    """The sounds played on events the engine handles, as playef arguments."""
    out = OrderedDict()
    alive(m, time=1000.0)
    tick(m, best=1001)
    out['new_record'] = m.calls(PLAYEF)
    alive(m, blocked=2.0)
    tick(m)
    out['die'] = m.calls(PLAYEF)
    alive(m, gameovertimer=9.0)
    tick(m)
    out['game_over'] = m.calls(PLAYEF)
    alive(m, gameovertimer=100.0, zoom=190, unlockevent=1)
    tick(m)
    out['unlock'] = m.calls(PLAYEF)
    alive(m, delayedsound=1, delayedsoundtimer=0.0)
    tick(m)
    out['title'] = m.calls(PLAYEF)
    m.reset(stage=-2, rotmode=-1)
    press(m, 'in_left')
    out['menu_move'] = m.calls(PLAYEF)
    m.reset(stage=-1, menuscreen=0, titlepage=0, menucursor=2, gameovertimer=100.0, zoom=320)
    press(m, 'in_select')
    out['menu_select'] = m.calls(PLAYEF)
    return out


def palette_roles(m):
    """Palettes the engine's own screens use: the menus, and the warning before deleting records."""
    out = OrderedDict()
    m.reset(stage=-2, menuselection=3, nsides=6, zoom=40, cursong=-1, expectedframedelta=1.0, rank=99,
            override_hexagoner=-1, override_hexagonest=-1)  # a locked level in the stage select
    tick(m)
    out['menu'] = m.calls(SETPAL)
    m.reset(stage=-1, menuscreen=1, gameovertimer=100.0, zoom=320)
    press(m, 'backlatch')
    out['warning'] = m.calls(SETPAL)
    for k, v in out.items():
        if len(v) != 1:
            raise RuntimeError('palette for %s: %r' % (k, v))
        out[k] = v[0]
    return out


def wall_markers(m):
    """Wall sides that aren't walls: what each does when it reaches the centre, as the field it
    sets. Sides 0..63 are tried."""
    found = OrderedDict()
    base = m.fields['enemies'][0]
    for side in range(64):
        alive(m, nenemies=1, speedramp=10.0, pausewaves=1)
        for k, v in (('side', side), ('dist', 1), ('len', 10), ('active', 1)):
            m.e.write(m.app + base + C.ENEMY[k], struct.pack('<i' if k != 'active' else '<B', v))
        tick(m)
        effects = [(f, m[f]) for f in ('sidechange', 'zoompulse') if m[f]]
        if effects:
            if len(effects) != 1:
                raise RuntimeError('wall side %d does %r' % (side, effects))
            found[side] = effects[0]
    return found


def roles(e, rnd):
    """Rank times, sounds and completion; engine event sounds; palette roles; wall markers."""
    m = Machine(e, rnd)
    try:
        times = rank_times(e)
        return times, ranks(m, times), sound_events(m), palette_roles(m), wall_markers(m)
    finally:
        m.release()


# --- rotation modes ------------------------------------------------------------------------------

def rotation_modes(e, rnd, modes):
    """What each rotation mode does to the playfield's spin (gameclass::updatevisualeffects), to
    the sway tilt (gameclass::otisrotate), and which way its spin bursts turn."""
    m = Machine(e, rnd)
    game = m.app + 0x18
    try:
        m.stub_callees('_ZN9gameclass19updatevisualeffectsER9helpclass')
        m.stub_callees('_ZN9gameclass10otisrotateEv')
        helper = e.alloc(0x100)

        def visual(n=1, **values):
            for k, v in values.items():
                m[k] = v
            for _ in range(n):
                e.call('_ZN9gameclass19updatevisualeffectsER9helpclass', game, helper)

        def settle(field, call, start, limit=1000):
            m[field] = start
            for _ in range(limit):
                before = m[field]
                call()
                if m[field] == before:
                    return before
            raise RuntimeError('%s does not settle' % field)

        out = OrderedDict()
        for mode in sorted(modes):
            m.reset(rotmode=mode, expectedframedelta=1.0, nsides=6)
            spins = []
            for start in (10.0, 100.0, 200.0):  # clear of the wrap at 0/360
                visual(spin=start)
                spins.append(m['spin'] - start)
            if len(set(spins)) == 1:
                entry = OrderedDict(spin=num(spins[0]))
            else:  # no spin of its own: the angle eases towards a fixed one
                rate = max(abs(s) for s in spins)
                target = settle('spin', lambda: visual(), 100.0)
                if settle('spin', lambda: visual(), 300.0) != target:
                    raise RuntimeError('rotation mode %d has no single resting angle' % mode)
                entry = OrderedDict(settle=num(target), settle_rate=num(rate))
            sway = settle('pitch', lambda: e.call('_ZN9gameclass10otisrotateEv', game), 0.0)
            entry['sway'] = num(sway)
            visual(spin=100.0, spinburst=1, spinburstvel=0.0)
            visual(5)
            entry['burst'] = 1 if m['spin'] > 100.0 else -1
            out[str(mode)] = entry
        return out
    finally:
        m.release()


def num(x):
    return int(x) if x == int(x) else x
