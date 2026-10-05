# DuperHex

A Super Hexagon game engine reimplementation in Rust + SDL3 + wgpu, plus scripts to extract assets from the latest Steam-on-Linux release of the game.

Asset extraction is nontrivial, since much of the game's "data" is encoded as if/else statements in code. `angr` and `unicorn` are used to recover and convert the relevant logic into a JSON-based representation.

Features:

- No FPS locking, with continuous-time physics and animation.
- F11 to toggle fullscreen mode.
- A new "extras" menu with the following new options:
	- Support for ultrawide aspect ratios without black bars.
	- FPS display.
	- Antialiasing options.
	- Chromatic aberration shader.
	- Bloom shader.
	- Adjust gameplay speed (highscores/progression will not persist when set below 1X).
- Fixes a palette-fade bug present in the original game (Can be seen in [this playthrough](https://www.youtube.com/watch?v=no88YA8vs2Q&t=438s) at 7:18).

The custom shaders are a little tacky, they're off by default and mainly exist as a demonstration of what's possible.

TODO:

- Input methods other than keyboard.

Non-Features:

- Steam integration, scoreboards, arcade mode.
