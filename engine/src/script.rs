//! The pack's expression/action language, with every name resolved
//! when the pack loads.
//!
//! Scripts are parsed by hand rather than through serde: the language is small, and this way a
//! mistake in one is reported as what it is.

use std::collections::{BTreeMap, HashMap};
use std::ops::{ControlFlow, Index, IndexMut};

use serde::Deserialize;
use serde_json::Value as Json;

use crate::ids::{LevelId, PatternId, RotationId, SoundId, TrackId};
use crate::pack::{Error, err};
use crate::weighted::Weighted;
use rand::rngs::SmallRng;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Val {
    Int(i64),
    Float(f64),
}

impl Val {
    pub fn f(self) -> f64 {
        match self {
            Val::Int(i) => i as f64,
            Val::Float(f) => f,
        }
    }

    pub fn truthy(self) -> bool {
        self.f() != 0.0
    }

    pub fn from_bool(b: bool) -> Val {
        Val::Int(b as i64)
    }

    pub fn from_json(j: &Json) -> Result<Val, Error> {
        match (j.as_i64(), j.as_f64()) {
            (Some(i), _) => Ok(Val::Int(i)),
            (None, Some(f)) => Ok(Val::Float(f)),
            _ => err(format!("not a number: {j}")),
        }
    }
}

/// The most counters the levels, the tutorial and the ending may declare between them.
pub const MAX_COUNTERS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CounterId(u8);

/// The counters a piece of logic runs with.
#[derive(Clone, Copy, Debug)]
pub struct Counters([Val; MAX_COUNTERS]);

impl Counters {
    pub const ZERO: Counters = Counters([Val::Int(0); MAX_COUNTERS]);
}

impl Index<CounterId> for Counters {
    type Output = Val;

    fn index(&self, c: CounterId) -> &Val {
        &self.0[c.0 as usize]
    }
}

impl IndexMut<CounterId> for Counters {
    fn index_mut(&mut self, c: CounterId) -> &mut Val {
        &mut self.0[c.0 as usize]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Var {
    Wave,
    Time,
    Elapsed,
    Speed,
    Sides,
    SidesAfterMorph,
    Morphing,
    Palette,
    PaletteFading,
    SpinBurstActive,
    Left,
    Right,
    Counter(CounterId),
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Not,
    Min,
    Max,
}

#[derive(Debug)]
pub enum Expr {
    Lit(Val),
    Var(Var),
    Op(Op, Vec<Expr>),
}

#[derive(Clone, Copy, Debug)]
pub enum Tilt {
    Random,
    Left,
    Right,
    Swing,
}

/// A step of a script: control flow, which the interpreter runs, or an effect on the world.
#[derive(Debug)]
pub enum Action {
    If(Expr, Vec<Action>, Vec<Action>),
    First(Vec<(Option<Expr>, Vec<Action>)>),
    Pick(Weighted<Vec<Action>>),
    Set(Var, Expr),
    Do(Effect),
}

#[derive(Debug)]
pub enum Effect {
    Pattern(PatternId),
    Delay(f64),
    RerollRotation(Vec<RotationId>),
    RandomRotation(Vec<RotationId>),
    Rotation(RotationId),
    Pulse(f64),
    SpinBurst,
    Tilt(Tilt),
    ZoomPulse,
    Shrink,
    Grow,
    Palette(Expr),
    Music(Option<TrackId>),
    Sfx(SoundId),
    Flash(f64),
    ClearWalls,
    FreezeWalls(f64),
    Camera { lean: Option<f64>, sway: Option<bool> },
    SwitchLevel { level: LevelId, time_offset: i64 },
    EndSequence,
    AdvanceWaves,
    EndTutorial,
}

/// Names a script may refer to, as the pack loads them.
pub struct Names {
    pub patterns: HashMap<String, PatternId>,
    pub tracks: HashMap<String, TrackId>,
    pub sounds: HashMap<String, SoundId>,
    pub levels: HashMap<String, LevelId>,
    pub counters: HashMap<String, CounterId>,
    /// By mode number.
    pub rotations: HashMap<i64, RotationId>,
}

impl Names {
    /// Numbers counters by name, in order of first appearance.
    pub fn number_counters<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<HashMap<String, CounterId>, Error> {
        let mut out = HashMap::new();
        for k in names {
            let n = out.len();
            out.entry(k.to_string()).or_insert(CounterId(n as u8));
        }
        if out.len() > MAX_COUNTERS {
            return err(format!("more than {MAX_COUNTERS} counters"));
        }
        Ok(out)
    }

    pub fn get<I: Copy>(map: &HashMap<String, I>, what: &str, name: &str) -> Result<I, Error> {
        map.get(name).copied().ok_or_else(|| Error(format!("no {what} {name:?}")))
    }

    pub fn rotation(&self, mode: i64) -> Result<RotationId, Error> {
        self.rotations.get(&mode).copied().ok_or_else(|| Error(format!("no rotation mode {mode}")))
    }

    pub fn counters(&self, values: &BTreeMap<String, Val>) -> Result<Counters, Error> {
        let mut out = Counters::ZERO;
        for (k, &v) in values {
            out[Self::get(&self.counters, "counter", k)?] = v;
        }
        Ok(out)
    }

    fn var(&self, name: &str) -> Result<Var, Error> {
        Ok(match name {
            "wave" => Var::Wave,
            "time" => Var::Time,
            "elapsed" => Var::Elapsed,
            "speed" => Var::Speed,
            "sides" => Var::Sides,
            "sides_after_morph" => Var::SidesAfterMorph,
            "morphing" => Var::Morphing,
            "palette" => Var::Palette,
            "palette_fading" => Var::PaletteFading,
            "spin_burst_active" => Var::SpinBurstActive,
            "left" => Var::Left,
            "right" => Var::Right,
            c => Var::Counter(Self::get(&self.counters, "variable or counter", c)?),
        })
    }

    pub fn expr(&self, j: &Json) -> Result<Expr, Error> {
        match j {
            Json::Number(_) => Ok(Expr::Lit(Val::from_json(j)?)),
            Json::String(s) => Ok(Expr::Var(self.var(s)?)),
            Json::Array(a) if !a.is_empty() => {
                let (op, arity) = match a[0].as_str().unwrap_or("") {
                    "+" => (Op::Add, 2..=2),
                    "-" => (Op::Sub, 2..=2),
                    "*" => (Op::Mul, 2..=2),
                    "/" => (Op::Div, 2..=2),
                    "%" => (Op::Rem, 2..=2),
                    "==" => (Op::Eq, 2..=2),
                    "!=" => (Op::Ne, 2..=2),
                    "<" => (Op::Lt, 2..=2),
                    "<=" => (Op::Le, 2..=2),
                    ">" => (Op::Gt, 2..=2),
                    ">=" => (Op::Ge, 2..=2),
                    "and" => (Op::And, 2..=usize::MAX),
                    "or" => (Op::Or, 2..=usize::MAX),
                    "not" => (Op::Not, 1..=1),
                    "min" => (Op::Min, 2..=usize::MAX),
                    "max" => (Op::Max, 2..=usize::MAX),
                    o => return err(format!("unknown operation {o:?}")),
                };
                if !arity.contains(&(a.len() - 1)) {
                    return err(format!("wrong number of arguments in {j}"));
                }
                Ok(Expr::Op(op, a[1..].iter().map(|x| self.expr(x)).collect::<Result<_, _>>()?))
            }
            _ => err(format!("bad expression {j}")),
        }
    }

    pub fn actions(&self, j: &Json) -> Result<Vec<Action>, Error> {
        match j {
            Json::Array(a) => a.iter().map(|x| self.action(x)).collect(),
            Json::Null => Ok(Vec::new()),
            Json::Object(_) => Ok(vec![self.action(j)?]),
            _ => err(format!("bad action list {j}")),
        }
    }

    fn weighted(&self, j: &Json) -> Result<(u32, Vec<Action>), Error> {
        match j[0].as_u64().and_then(|w| u32::try_from(w).ok()) {
            Some(w) => Ok((w, self.actions(&j[1])?)),
            None => err(format!("bad weight in {j}")),
        }
    }

    fn action(&self, j: &Json) -> Result<Action, Error> {
        fn as_name<'j>(verb: &str, v: &'j Json) -> Result<&'j str, Error> {
            v.as_str().ok_or_else(|| Error(format!("{verb}: not a name")))
        }
        fn as_list<'j>(verb: &str, v: &'j Json) -> Result<&'j Vec<Json>, Error> {
            v.as_array().ok_or_else(|| Error(format!("{verb}: not a list")))
        }

        let o = j.as_object().ok_or_else(|| Error(format!("bad action {j}")))?;
        if let Some(c) = o.get("if") {
            let branch = |k| o.get(k).map_or(Ok(Vec::new()), |b| self.actions(b));
            return Ok(Action::If(self.expr(c)?, branch("then")?, branch("else")?));
        }
        let (verb, v) = o.iter().next().ok_or_else(|| Error("empty action".into()))?;
        let name = |v| as_name(verb, v);
        let num = |v: &Json| v.as_f64().ok_or_else(|| Error(format!("{verb}: not a number")));
        let mode = |v: &Json| self.rotation(v.as_i64().ok_or_else(|| Error(format!("{verb}: bad mode")))?);
        let modes = |v: &Json| -> Result<Vec<RotationId>, Error> {
            let list = as_list(verb, v)?;
            if list.is_empty() {
                return err(format!("{verb}: no modes"));
            }
            list.iter().map(mode).collect()
        };
        let effect = match verb.as_str() {
            "pick" => {
                let entries = as_list(verb, v)?.iter().map(|e| self.weighted(e)).collect::<Result<Vec<_>, _>>()?;
                return Ok(Action::Pick(Weighted::new(entries)?));
            }
            "first" => {
                let entry = |e: &Json| Ok((e.get("when").map(|w| self.expr(w)).transpose()?, self.actions(&e["do"])?));
                return Ok(Action::First(as_list(verb, v)?.iter().map(entry).collect::<Result<_, Error>>()?));
            }
            "set" => {
                let var = self.var(name(&v[0])?)?;
                if !matches!(var, Var::Speed | Var::Counter(_)) {
                    return err(format!("can't set {var:?}"));
                }
                return Ok(Action::Set(var, self.expr(&v[1])?));
            }
            "pattern" => Effect::Pattern(Self::get(&self.patterns, "pattern", name(v)?)?),
            "delay" => Effect::Delay(num(v)?),
            "reroll_rotation" => Effect::RerollRotation(modes(v)?),
            "random_rotation" => Effect::RandomRotation(modes(v)?),
            "rotation" => Effect::Rotation(mode(v)?),
            "pulse" => Effect::Pulse(num(v)?),
            "spin_burst" => Effect::SpinBurst,
            "tilt" => Effect::Tilt(match name(v)? {
                "random" => Tilt::Random,
                "left" => Tilt::Left,
                "right" => Tilt::Right,
                "swing" => Tilt::Swing,
                t => return err(format!("unknown tilt {t}")),
            }),
            "zoom_pulse" => Effect::ZoomPulse,
            "morph" => match name(v)? {
                "grow" => Effect::Grow,
                "shrink" => Effect::Shrink,
                m => return err(format!("unknown morph {m}")),
            },
            "palette" => Effect::Palette(self.expr(v)?),
            "music" => Effect::Music(v.as_str().map(|t| Self::get(&self.tracks, "track", t)).transpose()?),
            "sfx" => Effect::Sfx(Self::get(&self.sounds, "sound", name(v)?)?),
            "flash" => Effect::Flash(num(v)?),
            "clear_walls" => Effect::ClearWalls,
            "freeze_walls" => Effect::FreezeWalls(num(v)?),
            "camera" => Effect::Camera { lean: v.get("lean").map(num).transpose()?, sway: v.get("sway").and_then(Json::as_bool) },
            "switch_level" => Effect::SwitchLevel {
                level: Self::get(&self.levels, "level", name(&v["level"])?)?,
                time_offset: v["time_offset"].as_i64().ok_or_else(|| Error("switch_level: bad time_offset".into()))?,
            },
            "end_sequence" => Effect::EndSequence,
            "advance_waves" => Effect::AdvanceWaves,
            "end_tutorial" => Effect::EndTutorial,
            _ => return err(format!("unknown action {verb}")),
        };
        Ok(Action::Do(effect))
    }
}

/// Whether the rest of a tick's logic runs: `Break` once a level switch has ended the old level's.
pub type Flow = ControlFlow<()>;

/// What the interpreter needs from the world: variables, effects, and its random numbers.
pub trait Host {
    fn get(&self, v: Var) -> Val;
    fn set(&mut self, v: Var, x: Val);
    fn act(&mut self, e: &'static Effect) -> Flow;
    fn rng(&mut self) -> &mut SmallRng;
}

pub fn eval(e: &Expr, h: &impl Host) -> Val {
    use Val::*;
    // integer arithmetic where it's defined, else as floats (so dividing by zero isn't fatal)
    let arith = |x: Val, y: Val, fi: fn(i64, i64) -> Option<i64>, ff: fn(f64, f64) -> f64| match (x, y) {
        (Int(a), Int(b)) => fi(a, b).map_or_else(|| Float(ff(a as f64, b as f64)), Int),
        _ => Float(ff(x.f(), y.f())),
    };
    match e {
        Expr::Lit(v) => *v,
        Expr::Var(v) => h.get(*v),
        Expr::Op(op, args) => {
            let a = |i: usize| eval(&args[i], h);
            match op {
                Op::Add => arith(a(0), a(1), i64::checked_add, |x, y| x + y),
                Op::Sub => arith(a(0), a(1), i64::checked_sub, |x, y| x - y),
                Op::Mul => arith(a(0), a(1), i64::checked_mul, |x, y| x * y),
                Op::Div => arith(a(0), a(1), i64::checked_div, |x, y| x / y),
                Op::Rem => arith(a(0), a(1), i64::checked_rem, |x, y| x % y),
                Op::Eq => Val::from_bool(a(0).f() == a(1).f()),
                Op::Ne => Val::from_bool(a(0).f() != a(1).f()),
                Op::Lt => Val::from_bool(a(0).f() < a(1).f()),
                Op::Le => Val::from_bool(a(0).f() <= a(1).f()),
                Op::Gt => Val::from_bool(a(0).f() > a(1).f()),
                Op::Ge => Val::from_bool(a(0).f() >= a(1).f()),
                Op::And => Val::from_bool(args.iter().all(|x| eval(x, h).truthy())),
                Op::Or => Val::from_bool(args.iter().any(|x| eval(x, h).truthy())),
                Op::Not => Val::from_bool(!a(0).truthy()),
                Op::Min => args.iter().map(|x| eval(x, h)).reduce(|x, y| if y.f() < x.f() { y } else { x }).unwrap(),
                Op::Max => args.iter().map(|x| eval(x, h)).reduce(|x, y| if y.f() > x.f() { y } else { x }).unwrap(),
            }
        }
    }
}

/// Runs an action list.
pub fn run(actions: &'static [Action], h: &mut impl Host) -> Flow {
    for a in actions {
        match a {
            Action::If(c, t, e) => run(if eval(c, h).truthy() { t } else { e }, h)?,
            Action::First(entries) => {
                if let Some((_, body)) = entries.iter().find(|(w, _)| w.as_ref().is_none_or(|w| eval(w, h).truthy())) {
                    run(body, h)?;
                }
            }
            Action::Pick(entries) => run(entries.pick(h.rng()), h)?,
            Action::Set(var, e) => {
                let v = eval(e, h);
                h.set(*var, v);
            }
            Action::Do(e) => h.act(e)?,
        }
    }
    Flow::Continue(())
}
