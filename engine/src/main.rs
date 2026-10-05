mod audio;
mod colour;
mod debug;
mod game;
mod gpu;
mod ids;
mod installed;
mod pack;
mod platform;
mod render;
mod save;
mod script;
mod text;
mod ui;
mod weighted;
mod world;

use std::error::Error;
use std::path::PathBuf;

use sdl3::event::Event;
use sdl3::keyboard::Scancode;

use game::Key;
use gpu::{Gpu, Vertex};
use world::TICK_RATE;

/// Longest stretch of real time a frame may simulate; the rest of a longer stall is skipped.
const MAX_FRAME_TICKS: f64 = 10.0;
/// How often the FPS counter updates.
const FPS_INTERVAL_NS: u64 = 500_000_000;

/// Maps SDL's clock to simulation time.
struct Clock {
    base: u64,
    sim0: f64,
    speed: f64,
}

impl Clock {
    fn at(&self, ns: u64) -> f64 {
        self.sim0 + ns.saturating_sub(self.base) as f64 * TICK_RATE * self.speed / 1e9
    }
}

/// The original's picture is 16:10; with black bars it keeps that shape.
const ORIGINAL_ASPECT: f64 = 1.6;

/// The frame's size and where it goes in a window of `w` x `h` pixels.
fn frame_rect(w: u32, h: u32, black_bars: bool) -> ((u32, u32), (u32, u32)) {
    if !black_bars {
        return ((w, h), (0, 0));
    }
    let (fw, fh) = if w as f64 / h as f64 > ORIGINAL_ASPECT {
        ((h as f64 * ORIGINAL_ASPECT).round() as u32, h)
    } else {
        (w, (w as f64 / ORIGINAL_ASPECT).round() as u32)
    };
    ((fw.max(1), fh.max(1)), ((w - fw) / 2, (h - fh) / 2))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("duperhex: {e}");
        std::process::exit(1);
    }
}

#[cfg(debug_assertions)]
const USAGE: &str = "usage: duperhex [PACK.zip | --pack ID] [--level ID] [--speed X]
       duperhex --install PACK.zip";
#[cfg(not(debug_assertions))]
const USAGE: &str = "usage: duperhex [PACK.zip | --pack ID] [--speed X]
       duperhex --install PACK.zip";

fn help() -> String {
    let mut s = format!(
        "{USAGE}

Arguments:
  PACK.zip      asset pack produced by the extractor (default: the latest installed)

Options:
  --install PACK.zip
                check a pack and install it for later runs, replacing any installed pack
                with the same id, then exit
  --pack ID     run the installed pack with this id
  --speed X     game speed multiplier override
  -h, --help    print this help and exit"
    );
    if cfg!(debug_assertions) {
        s += "

Debug options (debug builds only):
  --level ID    level to start in directly, skipping the menus

Debug environment variables (debug builds only):
  DUPERHEX_SHOT=DIR:T1,T2,...    save a screenshot to DIR at each tick time, then exit
  DUPERHEX_INPUT=T:KEYS,...      hold KEYS from each tick time: L R U D S(elect) E(sc) C(lear), or - for none
  DUPERHEX_TRACE                 print the player's state and the pulse every frame
  DUPERHEX_GOD                   make the player immune to walls";
    }
    s
}

/// Which pack to run.
enum PackChoice {
    Path(PathBuf),
    /// An installed pack, by id or else the latest.
    Installed(Option<String>),
}

/// The command line: a pack to install, or else the pack to run, a level to start in (debug
/// builds only), and a game speed overriding the saved one.
struct Args {
    install: Option<PathBuf>,
    pack: PackChoice,
    level: Option<String>,
    speed: Option<f64>,
}

fn args() -> Result<Args, String> {
    let mut path = None;
    let mut id = None;
    let mut install = None;
    let mut level = None;
    let mut speed = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "-h" || a == "--help" {
            println!("{}", help());
            std::process::exit(0);
        } else if a == "--install" {
            install = Some(PathBuf::from(it.next().ok_or(USAGE)?));
        } else if a == "--pack" {
            id = Some(it.next().ok_or(USAGE)?);
        } else if a == "--level" && cfg!(debug_assertions) {
            level = Some(it.next().ok_or(USAGE)?);
        } else if a == "--speed" {
            let v = it.next().ok_or(USAGE)?;
            speed = Some(v.parse().map_err(|_| format!("--speed: not a number: {v}"))?);
        } else if a.starts_with('-') || path.is_some() {
            return Err(USAGE.into());
        } else {
            path = Some(PathBuf::from(a));
        }
    }
    let pack = match (path, id) {
        (Some(_), Some(_)) => return Err(USAGE.into()),
        (Some(path), None) => PackChoice::Path(path),
        (None, id) => PackChoice::Installed(id),
    };
    Ok(Args { install, pack, level, speed })
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = args()?;
    if let Some(path) = &args.install {
        let (id, dest) = installed::install(path)?;
        println!("installed pack {id:?} to {}", dest.display());
        return Ok(());
    }
    let (path, chosen) = match args.pack {
        PackChoice::Path(path) => (path, None),
        PackChoice::Installed(id) => (installed::find(id.as_deref())?, id),
    };
    let (pack, files) = pack::Pack::load(&path)?;
    if let Some(id) = chosen {
        save::set_last_pack(&id);
    }
    let start = match &args.level {
        Some(id) => Some(pack.level(id).ok_or_else(|| format!("no level {id:?}"))?),
        None => None,
    };
    let font = text::Font::new(&pack.font)?;
    let mut dbg = debug::Debug::from_env();

    let sdl = sdl3::init()?;
    let video = sdl.video()?;
    let mut window = video.window("duperhex", 768, 480).resizable().high_pixel_density().build()?;
    let audio = audio::Audio::new(&sdl, pack, files)?;
    let mut events = sdl.event_pump()?;
    let mouse = sdl.mouse();
    let mut atlas = text::Atlas::new();

    let save = save::Save::load(&pack.id);
    // SAFETY: the window is declared first, so it's dropped after the Gpu.
    let mut gpu = unsafe { Gpu::new(&window, save.settings.vsync)? };
    if dbg.wants_shots() && !gpu.can_capture() {
        return Err("screenshots: the window's surface can't be read back".into());
    }
    let mut g = game::Game::new(pack, audio, save, gpu.sample_counts().to_vec(), args.speed, rand::random());
    g.set_god(dbg.god);
    if let Some(li) = start {
        g.start_run(li);
    }

    // frame buffers, reused
    let mut scene = render::Scene3d::default();
    let mut gui = ui::Gui::new(&font);
    let mut gui_verts: Vec<Vertex> = Vec::new();
    let mut text_verts: Vec<Vertex> = Vec::new();

    let mut clock = Clock { base: platform::ticks_ns(), sim0: g.world().t(), speed: g.speed() };
    // frame rate: frames counted since fps_start, shown as of the last update
    let (mut fps, mut fps_frames, mut fps_start) = (0.0, 0u32, clock.base);

    'run: loop {
        if g.take_display_changed() {
            let s = &g.save().settings;
            window.set_fullscreen(s.fullscreen).ok();
            mouse.show_cursor(!s.fullscreen);
            gpu.set_vsync(s.vsync);
        }
        let now = platform::ticks_ns();
        if g.speed() != clock.speed {
            clock = Clock { base: now, sim0: g.world().t(), speed: g.speed() };
        }
        if clock.at(now) - g.world().t() > MAX_FRAME_TICKS * clock.speed {
            clock = Clock { base: now, sim0: g.world().t() + MAX_FRAME_TICKS * clock.speed, ..clock };
        }
        let target = clock.at(now);

        for ev in events.poll_iter() {
            let (down, ts, sc) = match ev {
                Event::Quit { .. } => break 'run,
                Event::KeyDown { timestamp, scancode: Some(sc), repeat: false, .. } => (true, timestamp, sc),
                Event::KeyUp { timestamp, scancode: Some(sc), .. } => (false, timestamp, sc),
                _ => continue,
            };
            // apply input changes at the moment they happened
            g.advance_to(clock.at(ts).clamp(g.world().t(), target));
            let key = match sc {
                Scancode::Left | Scancode::A => Key::Left,
                Scancode::Right | Scancode::D => Key::Right,
                Scancode::Up | Scancode::W => Key::Up,
                Scancode::Down | Scancode::S => Key::Down,
                Scancode::Space | Scancode::Return | Scancode::Z => Key::Select,
                Scancode::Escape => Key::Quit,
                Scancode::C => Key::Clear,
                Scancode::F11 if down => {
                    g.toggle_fullscreen();
                    continue;
                }
                _ => continue,
            };
            g.key(key, down);
        }
        dbg.feed(&mut g, target);
        g.advance_to(target);
        if g.quit {
            break;
        }
        dbg.trace(&g);

        let (w, h) = window.size_in_pixels();
        gpu.resize((w, h));
        let settings = &g.save().settings;
        let ((fw, fh), at) = frame_rect(w, h, settings.black_bars);
        scene.build(g.world(), fw as f64, fh as f64);
        // the interface, at fh / GUI_H pixels per unit
        let k = fh as f64 / ui::GUI_H;
        gui.build(&g, fw as f64 / k, render::palette(g.world()));
        if settings.show_fps {
            fps_frames += 1;
            let dt = now.saturating_sub(fps_start);
            if dt >= FPS_INTERVAL_NS {
                fps = fps_frames as f64 * 1e9 / dt as f64;
                (fps_frames, fps_start) = (0, now);
            }
            gui.fps(fps);
        }
        gui_verts.clear();
        gui_verts.extend(gui.tris.iter().flat_map(|t| {
            let color = colour::Linear::from_srgb(t.color, 1.0);
            t.pts.map(|(x, y)| Vertex { pos: [(x * k) as f32, (y * k) as f32], uv: [0.0; 2], color })
        }));
        text_verts.clear();
        atlas.build(&font, &gui.arena, &gui.texts, k, &mut text_verts);
        let t = g.world().t();
        let frame = gpu::Frame {
            clear: scene.clear,
            scene: &scene.verts,
            player: &scene.player,
            gui: &gui_verts,
            text: &text_verts,
            glyphs: &mut atlas.uploads,
            size: (fw, fh),
            at,
            samples: settings.antialiasing,
            aberration: if settings.aberration { render::aberration(g.world()) } else { 0.0 },
            bloom: if settings.bloom { render::bloom(g.world()) } else { 0.0 },
            backdrop: scene.backdrop,
        };
        if let Some(image) = gpu.render(frame, dbg.shot_due(t))
            && dbg.save_shot(&image, t)
        {
            break 'run;
        }
    }
    Ok(())
}
