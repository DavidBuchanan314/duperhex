//! The SDL calls the sdl3 crate doesn't wrap, in one place.

use sdl3::audio::AudioStream;
use sdl3::render::WindowCanvas;

/// SDL's clock, in nanoseconds: the timebase of input events' timestamps.
pub fn ticks_ns() -> u64 {
    // SAFETY: no preconditions.
    unsafe { sdl3::sys::timer::SDL_GetTicksNS() }
}

pub fn set_vsync(canvas: &mut WindowCanvas, on: bool) {
    // SAFETY: the renderer is live for the canvas's lifetime.
    unsafe {
        sdl3::sys::render::SDL_SetRenderVSync(canvas.raw(), on as i32);
    }
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
