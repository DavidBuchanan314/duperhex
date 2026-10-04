mod audio;
mod debug;
mod game;
mod ids;
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

use sdl3::event::Event;
use sdl3::keyboard::Scancode;
use sdl3::pixels::PixelFormat;
use sdl3::render::{FPoint, FRect, ScaleMode, Texture, Vertex, VertexIndices};

use game::Key;
use world::TICK_RATE;

/// Longest stretch of real time a frame may simulate; longer stalls are skipped.
const MAX_FRAME_TICKS: f64 = 10.0;
/// Antialiasing: the scene is drawn at 2^SS_HALVINGS times the window resolution and halved with
/// linear filtering until it fits, each halving averaging 2x2 pixels.
const SS_HALVINGS: u32 = 2;
const MAX_SS_SIZE: u32 = 8192;
/// How often the FPS counter updates.
const FPS_INTERVAL_NS: u64 = 500_000_000;

/// Supersampling render targets, largest first, and the window size they were made for.
struct Supersample<'a> {
    targets: Vec<Texture<'a>>,
    size: (u32, u32),
}

fn main() {
    if let Err(e) = run() {
        eprintln!("duperhex: {e}");
        std::process::exit(1);
    }
}

#[cfg(debug_assertions)]
const USAGE: &str = "usage: duperhex PACK.zip [LEVEL_ID] [--speed X] [--fps]";
#[cfg(not(debug_assertions))]
const USAGE: &str = "usage: duperhex PACK.zip [--speed X] [--fps]";

fn help() -> String {
    let mut s = format!(
        "{USAGE}

Arguments:
  PACK.zip      asset pack produced by the extractor

Options:
  --speed X     game speed multiplier, from {} to {} (default 1)
  --fps         show a frame rate counter
  -h, --help    print this help and exit",
        audio::MIN_SPEED,
        audio::MAX_SPEED
    );
    if cfg!(debug_assertions) {
        s += "

Debug arguments (debug builds only):
  LEVEL_ID      level to start in directly, skipping the menus

Debug environment variables (debug builds only):
  DUPERHEX_SHOT=DIR:T1,T2,...    save a screenshot to DIR at each tick time, then exit
  DUPERHEX_INPUT=T:KEYS,...      hold KEYS from each tick time: L R U D S(elect) E(sc) C(lear), or - for none
  DUPERHEX_TRACE                 print the player's state and the pulse every frame
  DUPERHEX_GOD                   make the player immune to walls";
    }
    s
}

/// The command line: the pack, a level to start in (debug builds only), how fast the game runs,
/// and whether to show the frame rate.
struct Args {
    pack: String,
    level: Option<String>,
    speed: f64,
    show_fps: bool,
}

fn args() -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut speed = 1.0;
    let mut show_fps = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "-h" || a == "--help" {
            println!("{}", help());
            std::process::exit(0);
        } else if a == "--speed" {
            let v = it.next().ok_or(USAGE)?;
            speed = v.parse().map_err(|_| format!("--speed: not a number: {v}"))?;
            if !(audio::MIN_SPEED..=audio::MAX_SPEED).contains(&speed) {
                return Err(format!("--speed: must be from {} to {}", audio::MIN_SPEED, audio::MAX_SPEED));
            }
        } else if a == "--fps" {
            show_fps = true;
        } else {
            positional.push(a);
        }
    }
    let mut positional = positional.into_iter();
    let pack = positional.next().ok_or(USAGE)?;
    let level = if cfg!(debug_assertions) { positional.next() } else { None };
    if positional.next().is_some() {
        return Err(USAGE.into());
    }
    Ok(Args { pack, level, speed, show_fps })
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = args()?;
    let (pack, files) = pack::Pack::load(&args.pack)?;
    let start = match &args.level {
        Some(id) => Some(pack.level(id).ok_or_else(|| format!("no level {id:?}"))?),
        None => None,
    };
    let font = text::Font::new(&pack.font)?;
    let mut dbg = debug::Debug::from_env();

    let sdl = sdl3::init()?;
    let video = sdl.video()?;
    let window = video.window("duperhex", 768, 480).resizable().high_pixel_density().build()?;
    let mut canvas = window.into_canvas();
    let audio = audio::Audio::new(&sdl, pack, files, args.speed)?;
    let mut events = sdl.event_pump()?;
    let mouse = sdl.mouse();
    let creator = canvas.texture_creator();
    let mut atlas = text::Atlas::new(&creator)?;
    let mut ss = Supersample { targets: Vec::new(), size: (0, 0) };

    let save = save::Save::load(&pack.id);
    let mut g = game::Game::new(pack, audio, save, rand::random());
    g.set_god(dbg.god);
    if let Some(li) = start {
        g.start_run(li);
    }

    // frame buffers, reused
    let mut scene = render::Scene3d::default();
    let mut gui = ui::Gui::new(&font);
    let mut gui_verts: Vec<Vertex> = Vec::new();
    let mut text_verts: Vec<Vertex> = Vec::new();

    // simulation time = (real ns - base) in ticks
    let mut base = platform::ticks_ns();
    let speed = args.speed;
    let to_sim = |ns: u64, base: u64| ns.saturating_sub(base) as f64 * TICK_RATE * speed / 1e9;
    // frame rate: frames counted since fps_start, shown as of the last update
    let (mut fps, mut fps_frames, mut fps_start) = (0.0, 0u32, base);

    'run: loop {
        if g.take_display_changed() {
            let s = &g.save().settings;
            canvas.window_mut().set_fullscreen(s.fullscreen).ok();
            mouse.show_cursor(!s.fullscreen);
            platform::set_vsync(&mut canvas, s.vsync);
        }
        let now = platform::ticks_ns();
        if to_sim(now, base) - g.world().t() > MAX_FRAME_TICKS * speed {
            base = now - (g.world().t() * 1e9 / (TICK_RATE * speed)) as u64;
        }
        let target = to_sim(now, base);

        for ev in events.poll_iter() {
            let (down, ts, sc) = match ev {
                Event::Quit { .. } => break 'run,
                Event::KeyDown { timestamp, scancode: Some(sc), repeat: false, .. } => (true, timestamp, sc),
                Event::KeyUp { timestamp, scancode: Some(sc), .. } => (false, timestamp, sc),
                _ => continue,
            };
            // apply input changes at the moment they happened
            g.advance_to(to_sim(ts, base).clamp(g.world().t(), target));
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

        let (w, h) = canvas.output_size()?;
        if ss.size != (w, h) {
            let mut halvings = SS_HALVINGS;
            while halvings > 0 && w.max(h) << halvings > MAX_SS_SIZE {
                halvings -= 1;
            }
            let mut targets = Vec::new();
            for k in (1..=halvings).rev() {
                let mut t = creator.create_texture_target(PixelFormat::RGBA32, w << k, h << k)?;
                t.set_scale_mode(ScaleMode::Linear);
                targets.push(t);
            }
            ss = Supersample { targets, size: (w, h) };
        }
        let (sw, sh) = (w << ss.targets.len(), h << ss.targets.len());
        scene.build(g.world(), sw as f64, sh as f64);
        // the interface, at sh / GUI_H pixels per unit
        let k = sh as f64 / ui::GUI_H;
        gui.build(&g, sw as f64 / k, render::palette(g.world()));
        if args.show_fps {
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
            let color = render::fcolor(t.color);
            t.pts.map(|(x, y)| Vertex { position: FPoint::new((x * k) as f32, (y * k) as f32), color, tex_coord: FPoint::new(0.0, 0.0) })
        }));
        // text goes on top at the window's resolution: its glyphs are already antialiased, and
        // supersampled ones would be too big for the atlas
        text_verts.clear();
        atlas.build(&font, &gui.arena, &gui.texts, h as f64 / ui::GUI_H, &mut text_verts);
        let draw = |c: &mut sdl3::render::WindowCanvas| {
            c.set_draw_color(scene.clear);
            c.clear();
            // drawing errors only lose a frame
            c.render_geometry(&scene.verts, None, VertexIndices::Sequential).ok();
            c.render_geometry(&gui_verts, None, VertexIndices::Sequential).ok();
        };
        if ss.targets.is_empty() {
            draw(&mut canvas);
        } else {
            canvas.with_texture_canvas(&mut ss.targets[0], draw)?;
            for i in 1..ss.targets.len() {
                let (big, small) = ss.targets.split_at_mut(i);
                canvas.with_texture_canvas(&mut small[0], |c| {
                    c.copy(&big[i - 1], None, None).ok();
                })?;
            }
            canvas.copy(ss.targets.last().unwrap(), None, FRect::new(0.0, 0.0, w as f32, h as f32))?;
        }
        canvas.render_geometry(&text_verts, Some(&atlas.tex), VertexIndices::Sequential).ok();
        if dbg.shoot(&canvas, g.world().t()) {
            break 'run;
        }
        canvas.present();
    }
    Ok(())
}
