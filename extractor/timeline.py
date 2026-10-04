"""A level's progression as a timeline, from its lifted per-tick logic (see logic.py).

The per-tick logic of a level depends only on the clock and on the palette, so it can be run
ahead tick by tick from the level's start, recording what it does when. The camera, which the
original sets every tick, becomes a level setting; the wave timer it advances becomes the engine's
business.

One simplification: the original's palette changes also wait for any palette fade in progress to
end. Fades follow a colour oscillator that runs from the game's launch, not from the start of a
run, so that wait has no fixed time even in the original; the timeline fires them on time. Nothing
that changes the walls waits on a fade.
"""
from collections import OrderedDict

# The longest a level runs without switching: the original's levels have nothing after 3 minutes
# that isn't a level switch.
HORIZON = 4 * 3600


def run(on_tick, start_time, palette, counters):
    """Returns (camera, timeline): the camera settings and [(time, [actions])]."""
    state = dict(counters, time=start_time, palette=palette, palette_fading=0)
    camera = None
    entries = []
    for t in range(start_time + 1, start_time + HORIZON):
        state['time'] = t
        done = []
        cam = OrderedDict()
        switched = act_list(on_tick, state, done, cam)
        if camera is None:
            camera = cam
        elif cam != camera:
            raise RuntimeError('camera changes over time: %r then %r' % (camera, cam))
        if done:
            entries.append((t, done))
        if switched:
            break
    return camera or OrderedDict(), entries


def act_list(actions, state, done, cam):
    """Runs actions; returns True if the level switched (which ends its logic)."""
    for a in actions:
        if 'if' in a:
            branch = a.get('then', []) if truthy(ev(a['if'], state)) else a.get('else', [])
            if act_list(branch, state, done, cam):
                return True
        elif 'first' in a:
            for entry in a['first']:
                if 'when' not in entry or truthy(ev(entry['when'], state)):
                    if act_list(entry['do'], state, done, cam):
                        return True
                    break
        elif 'set' in a:
            name, e = a['set']
            if name not in state or name in ('time', 'palette', 'palette_fading'):
                raise RuntimeError('level logic sets %r' % name)
            state[name] = ev(e, state)
        elif 'camera' in a:
            cam.update(a['camera'])
        elif 'advance_waves' in a:
            pass
        elif 'palette' in a:
            p = ev(a['palette'], state)
            state['palette'] = p
            done.append(OrderedDict(palette=p))
        else:
            done.append(a)
            if 'switch_level' in a:
                return True
    return False


def truthy(v):
    return v != 0


def ev(e, state):
    if isinstance(e, (int, float)):
        return e
    if isinstance(e, str):
        if e not in state:
            raise RuntimeError('level logic reads %r, which depends on more than the clock' % e)
        return state[e]
    op, args = e[0], [ev(x, state) for x in e[1:]]
    if op == 'and':
        return int(all(truthy(x) for x in args))
    if op == 'or':
        return int(any(truthy(x) for x in args))
    if op == 'not':
        return int(not truthy(args[0]))
    a, b = args[0], args[1] if len(args) > 1 else None
    if op == '%':
        r = abs(a) % abs(b)
        return r if a >= 0 else -r
    if op == '/':
        return int(a / b) if isinstance(a, int) and isinstance(b, int) else a / b
    return {
        '+': lambda: a + b, '-': lambda: a - b, '*': lambda: a * b,
        '==': lambda: int(a == b), '!=': lambda: int(a != b), '<': lambda: int(a < b),
        '<=': lambda: int(a <= b), '>': lambda: int(a > b), '>=': lambda: int(a >= b),
        'min': lambda: min(args), 'max': lambda: max(args),
    }[op]()
