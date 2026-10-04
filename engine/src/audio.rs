//! Sound, mixed by SDL: a stream per sound effect and per music track, each in its own format,
//! bound to the playback device. The music is fed ahead a little at a time from the game's tick,
//! which also steps its fades.

use std::io::Cursor;
use std::sync::{Arc, OnceLock};

use sdl3::AudioSubsystem;
use sdl3::audio::{AudioDevice, AudioFormat, AudioSpec, AudioStreamOwner};

use crate::ids::{IdVec, SoundId, TrackId};
use crate::pack::{Files, Pack};
use crate::save::MAX_VOLUME;

/// How far ahead the music is fed, in seconds of real time.
const MUSIC_LEAD: f64 = 1.0;
/// How fast the game can run, as SDL can speed sound up or slow it down.
pub const MIN_SPEED: f64 = 0.01;
pub const MAX_SPEED: f64 = 100.0;
/// Music fades in over this many ticks, and out over FADE_OUT.
const FADE_IN: u32 = 30;
const FADE_OUT: u32 = 45;

/// Decoded audio: interleaved 16-bit samples.
struct Pcm {
    samples: Vec<i16>,
    channels: usize,
    rate: usize,
}

impl Pcm {
    fn spec(&self) -> AudioSpec {
        AudioSpec::new(Some(self.rate as i32), Some(self.channels as i32), Some(AudioFormat::s16_sys()))
    }

    fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    /// Frames' worth of a stream's queued bytes.
    fn queued_frames(&self, stream: &AudioStreamOwner) -> usize {
        stream.queued_bytes().unwrap_or(0) as usize / (size_of::<i16>() * self.channels)
    }
}

fn decode(name: &str, bytes: &[u8]) -> Pcm {
    let read = || -> Result<Pcm, lewton::VorbisError> {
        let mut r = lewton::inside_ogg::OggStreamReader::new(Cursor::new(bytes))?;
        let (channels, rate) = (r.ident_hdr.audio_channels as usize, r.ident_hdr.audio_sample_rate as usize);
        let mut samples = Vec::new();
        while let Some(p) = r.read_dec_packet_itl()? {
            samples.extend(p);
        }
        Ok(Pcm { samples, channels, rate })
    };
    read().unwrap_or_else(|e| {
        eprintln!("{name}: {e}");
        Pcm { samples: Vec::new(), channels: 1, rate: 44100 }
    })
}

struct Music {
    track: TrackId,
    /// The frame fed next, and where the track ends (or loops).
    fed: usize,
    end: usize,
    looping: bool,
    /// Ticks into the fade-in, and the ticks left of the fade-out once fading out.
    fade_in: u32,
    fade_out: Option<u32>,
}

pub struct Audio {
    pack: &'static Pack,
    subsystem: AudioSubsystem,
    sounds: IdVec<SoundId, (Pcm, AudioStreamOwner)>,
    /// Decoded in the background as the game starts.
    tracks: Arc<IdVec<TrackId, OnceLock<Pcm>>>,
    /// Made when a track first plays (it may still be decoding before then).
    track_streams: IdVec<TrackId, Option<AudioStreamOwner>>,
    music: Option<Music>,
    music_gain: f32,
    /// How fast the game runs, and so everything plays.
    speed: f64,
    /// Closed after the streams are dropped (which unbinds them).
    device: AudioDevice,
}

impl Audio {
    pub fn new(sdl: &sdl3::Sdl, pack: &'static Pack, mut files: Files, speed: f64) -> Result<Audio, sdl3::Error> {
        let tracks: Arc<IdVec<_, _>> = Arc::new(pack.tracks.iter().map(|_| OnceLock::new()).collect());
        for (id, t) in pack.tracks.iter_enumerated() {
            let (tracks, file, bytes) = (tracks.clone(), t.file.as_str(), files.take(&t.file));
            std::thread::spawn(move || {
                let _ = tracks[id].set(decode(file, &bytes));
            });
        }
        let subsystem = sdl.audio()?;
        let device = subsystem.open_playback_device(&AudioSpec::new(None, None, None))?;
        let sounds = pack
            .sounds
            .iter()
            .map(|s| {
                let pcm = decode(&s.file, &files.take(&s.file));
                let stream = subsystem.new_stream(Some(&pcm.spec()), None)?;
                crate::platform::set_frequency_ratio(&stream, speed as f32)?;
                device.bind_stream(&stream)?;
                Ok((pcm, stream))
            })
            .collect::<Result<_, sdl3::Error>>()?;
        let track_streams = pack.tracks.iter().map(|_| None).collect();
        Ok(Audio { pack, subsystem, sounds, tracks, track_streams, music: None, music_gain: 1.0, speed, device })
    }

    /// Volumes from 0 to MAX_VOLUME.
    pub fn set_volumes(&mut self, music: u32, sound: u32) {
        self.music_gain = music as f32 / MAX_VOLUME as f32;
        for (_, s) in &self.sounds {
            s.set_gain(sound as f32 / MAX_VOLUME as f32).ok();
        }
    }

    /// Plays a sound from its start (cutting it off if it was already playing).
    pub fn sfx(&self, id: SoundId) {
        let (pcm, stream) = &self.sounds[id];
        stream.clear().ok();
        stream.put_data_i16(&pcm.samples).ok();
    }

    pub fn play_music(&mut self, track: TrackId, offset_ms: f64) {
        self.stop_music();
        let t = &self.pack.tracks[track];
        let pcm = self.tracks[track].wait();
        let frames = |ms: f64| (ms * pcm.rate as f64 / 1000.0) as usize;
        let end = frames(t.length_ms).min(pcm.frames());
        if self.track_streams[track].is_none() {
            let stream = self.subsystem.new_stream(Some(&pcm.spec()), None).and_then(|s| {
                crate::platform::set_frequency_ratio(&s, self.speed as f32)?;
                self.device.bind_stream(&s)?;
                Ok(s)
            });
            match stream {
                Ok(s) => self.track_streams[track] = Some(s),
                Err(e) => {
                    eprintln!("music: {e}");
                    return;
                }
            }
        }
        self.music = Some(Music { track, fed: frames(offset_ms).min(end), end, looping: t.looping, fade_in: 0, fade_out: None });
        self.tick();
    }

    pub fn stop_music(&mut self) {
        if let Some(m) = self.music.take()
            && let Some(s) = &self.track_streams[m.track]
        {
            s.clear().ok();
        }
    }

    pub fn fade_out_music(&mut self) {
        if let Some(m) = &mut self.music {
            // from a tick past full volume
            m.fade_out.get_or_insert(FADE_OUT + 1);
        }
    }

    /// Steps the music's fades and feeds it ahead. Called every tick.
    pub fn tick(&mut self) {
        let Some(m) = &mut self.music else { return };
        let (Some(stream), Some(pcm)) = (&self.track_streams[m.track], self.tracks[m.track].get()) else { return };

        let mut volume = m.fade_in as f32 / FADE_IN as f32;
        m.fade_in = (m.fade_in + 1).min(FADE_IN);
        if let Some(f) = &mut m.fade_out {
            if *f == 0 {
                stream.clear().ok();
                self.music = None;
                return;
            }
            volume = volume.min((*f as f32 / FADE_OUT as f32).min(1.0));
            *f -= 1;
        }
        stream.set_gain(volume * self.music_gain).ok();

        // the track's own frames: at speed, more of them play per second
        let lead = (MUSIC_LEAD * self.speed * pcm.rate as f64) as usize;
        let mut want = lead.saturating_sub(pcm.queued_frames(stream));
        while want > 0 {
            if m.fed >= m.end {
                if !m.looping {
                    break;
                }
                m.fed = 0;
            }
            let n = want.min(m.end - m.fed);
            stream.put_data_i16(&pcm.samples[m.fed * pcm.channels..(m.fed + n) * pcm.channels]).ok();
            m.fed += n;
            want -= n;
        }
    }

    /// The playing track and how far into it, in milliseconds.
    pub fn music_position(&self) -> Option<(TrackId, f64)> {
        let m = self.music.as_ref()?;
        let (stream, pcm) = (self.track_streams[m.track].as_ref()?, self.tracks[m.track].get()?);
        let queued = pcm.queued_frames(stream);
        // what's been fed, less what's still queued: back across the loop point if need be
        let frame = if queued <= m.fed { m.fed - queued } else { m.end - (queued - m.fed) };
        Some((m.track, frame as f64 * 1000.0 / pcm.rate as f64))
    }
}
