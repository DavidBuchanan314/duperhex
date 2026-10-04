# DuperHex

A Super Hexagon game engine reimplementation, plus scripts to extract assets from the latest Steam-on-Linux release of the game.

Asset extraction is nontrivial, since much of the game's "data" is encoded as if/else statements in code. `angr` and `unicorn` are used to recover and convert the relevant logic into a JSON-based representation.

Features:

- No FPS locking, with continuous-time physics and animation.
- Support for ultrawide aspect ratios without black bars.
- F11 to toggle fullscreen mode.
- Optional FPS counter.

TODO:

- Input methods other than keyboard.

Non-Features:

- Steam integration, scoreboards, arcade mode.
