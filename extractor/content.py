"""Struct layouts of this build of the binary: where the fields the extractor reads and sets
live. Everything in the pack is recovered from the game's own code and data.
"""

# --- struct layout (this build) -----------------------------------------------------------------

GAMECLASS_SIZE = 0xbfb0
GAME = {
    'enemies': 0x168, 'nenemies': 0x2878, 'wavetimer': 0x28b4, 'speed': 0x28c0,
    'speedramp': 0x28c8, 'pausewaves': 0x5464, 'nsides': 0xfc, 'credits': 0x55a0,
    'stage': 0x540c, 'menuscreen': 0x5448, 'menuselection': 0x544c, 'won': 0x5418, 'zoom': 0xf4,
    'gameovertimer': 0xec, 'timetable': 0x5480, 'inputtype': 0x5548, 'touchlayout': 0x3c, 'unlockevent': 0x556c, 'unlockeventtimer': 0x5574, 'hyper': 0x5460,
    'titlepage': 0xbfa4, 'menucursor': 0xbfa8, 'arcademode': 0x57b8, 'prompts': 0x54a8,
}
GRAPHICS = {'screenw': 0x0, 'ui': 0x6c, 'skewflip': 0x2754, 'curpal_r': 0x26d0, 'curpal_g': 0x26f8, 'curpal_b': 0x2720}
HELP = {'glow': 0x60, 'slowsine': 0x64}
SUPERHEX = {'game': 0x18, 'graphics': 0xbfc8, 'mouseclicked': 0x3c464, 'touchx': 0x3c468, 'touchy': 0x3c490}
# std::string members, (offset, count), which must hold valid strings before the GUI code runs
GAME_STRINGS = [(0x48, 7), (0xb8, 1), (0xd8, 1), (0x118, 1), (0x54a8, 20), (0x5620, 30), (0x57c8, 1), (0x9870, 1)]
GRAPHICS_STRINGS = [(0x22d0, 1), (0x121c0, 100)]
ENEMY_SIZE = 0x14
GRAPHICS_SIZE = 0x124e0
PALETTE_ARRAYS = 0x25e0   # startpal_r, _g, _b, endpal_r, _g, _b: int[10] each
MUSICCLASS_SIZE = 0x1da88
MUSIC = {'beattable': 0x18, 'songs': 0x1d4e0, 'cursong': 0x1da60, 'playcount': 0x1da64, 'songlen_ms': 0x1da80}
BEATTABLE_LEN = 30001
SOUNDPLAYER_SIZE = 0x40

# superhex fields read or written by superhex::gamelogic: name -> (offset in superhex, kind),
# kind i int, f float, d double, b bool, l int64. game is at +0x18, graphics at +0xbfc8, music at +0x1e4a8.
SUPERHEX_SIZE = 0x3c758
SUPERHEX_FIELDS = {
    'stage': (0x5424, 'i'), 'rank': (0x293c, 'i'), 'zoom': (0x10c, 'i'), 'rotmode': (0x2900, 'i'),
    'override_hexagonest': (0x55dc, 'i'), 'override_hexagoner': (0x55e4, 'i'), 'temp': (0x13c, 'i'),
    'postwin': (0x546c, 'i'), 'zoompulse': (0x53f8, 'i'), 'unusedcc': (0xe4, 'i'), 'sidechange': (0x170, 'i'),
    'pausewaves': (0x547c, 'i'), 'fadestate': (0x110, 'i'), 'beat': (0x2924, 'i'), 'timelinestate': (0x5618, 'i'),
    'spinburst': (0x5410, 'i'), 'wobble': (0x5404, 'i'), 'wavecount': (0x28d0, 'i'), 'hyperopening': (0x5474, 'i'),
    'unlockevent': (0x5584, 'i'), 'nenemies': (0x2890, 'i'), 'nsides': (0x114, 'i'), 'unused160': (0x178, 'i'),
    'time': (0x2894, 'f'), 'speedramp': (0x28e0, 'f'), 'gameovertimer': (0x104, 'f'), 'blocked': (0x28f8, 'f'),
    'sidechangefreeze': (0x5420, 'f'), 'fadetimer': (0xe8, 'f'), 'speedrampbrake': (0x28d4, 'f'),
    'wavetimer': (0x28cc, 'f'), 'fade': (0x128, 'f'), 'pulse': (0x2928, 'f'), 'timelinedelay': (0x5630, 'f'),
    'speed': (0x28d8, 'f'), 'shapewavecounter': (0x17c, 'f'), 'expectedframedelta': (0x28c0, 'd'),
    'hyper': (0x5478, 'b'), 'tutorial': (0x5575, 'b'), 'timelinestart_ms': (0x5628, 'l'),
    'timeline_ms': (0x5620, 'l'), 'pal': (0xe714, 'i'), 'palstate': (0xe710, 'i'), 'paltarget': (0xe718, 'i'),
    'cursong': (0x3bf08, 'i'), 'now_ms': (0x3c710, 'l'),
    'tutorialstate': (0x5578, 'i'), 'tutorialtimer': (0x5580, 'f'), 'menuslide': (0x5470, 'f'),
    'tutorialflag': (0x557c, 'i'), 'in_left': (0x43, 'b'), 'in_right': (0x44, 'b'),
}

# further fields used to set up and start levels (see levels.py)
SETUP_FIELDS = {
    'in_select': (0x45, 'b'), 'tutorialflag': (0x557c, 'i'), 'centerflip': (0x5414, 'i'), 'turnspeed': (0x28f4, 'i'),
    'menuselection': (0x5464, 'i'), 'won': (0x5430, 'i'), 'menumovecooldown': (0xbc, 'f'),
    'in_left': (0x43, 'b'), 'backlatch': (0x49, 'b'), 'menuscreen': (0x5460, 'i'), 'titlepage': (0xbfbc, 'i'),
    'menucursor': (0xbfc0, 'i'), 'delayedsound': (0x5600, 'b'), 'delayedsoundtimer': (0x5604, 'f'),
    'levelreached': (0x2938, 'i'), 'attached': (0xe5a4, 'b'), 'enemies': (0x180, 'i'),
    'spin': (0x11c, 'f'), 'pitch': (0x5428, 'f'), 'spinburstvel': (0x5418, 'f'),
}
ENEMY = {'side': 0, 'dist': 4, 'len': 8, 'active': 16}


