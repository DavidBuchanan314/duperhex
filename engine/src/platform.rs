//! The SDL calls the sdl3 crate doesn't wrap, in one place.

use sdl3::audio::AudioStream;

/// SDL's clock, in nanoseconds: the timebase of input events' timestamps.
pub fn ticks_ns() -> u64 {
    // SAFETY: no preconditions.
    unsafe { sdl3::sys::timer::SDL_GetTicksNS() }
}

/// Plays a stream's audio `ratio` times as fast (and pitched up as much), from 0.01 to 100.
pub fn set_frequency_ratio(stream: &AudioStream, ratio: f32) -> Result<(), sdl3::Error> {
    // SAFETY: the stream is live.
    if unsafe { sdl3::sys::audio::SDL_SetAudioStreamFrequencyRatio(stream.raw(), ratio) } {
        Ok(())
    } else {
        Err(sdl3::get_error())
    }
}
