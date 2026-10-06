//! layerExParticle.dll: per-Layer simulation, script initializers and GPU sprites.
mod model;
mod render;
use crate::exports::{arg, object};
use krkr_engine::{
    extensions,
    protocol::budget::{Budget, Permit},
};
use model::{Model, Particle, error};
use std::{cell::Cell, rc::Rc};
use tjs_bind::flow;
use tjs_core::{
    NativeCallable as Call, NativeCx, NativeError, NativeProperty, NativeResult, NativeStep, ObjId,
    Trace, Value, value,
};

struct State {
    model: Option<Model>,
    image: Value,
    initializer: Value,
    mode: i32,
    frames: Vec<[i32; 4]>,
    frame_permit: Option<Permit>,
    old: Vec<krkr_engine::protocol::graphics::Rect>,
    old_permit: Option<Permit>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            model: None,
            image: Value::Obj(Default::default()),
            initializer: Value::Obj(Default::default()),
            mode: 2,
            frames: Vec::new(),
            frame_permit: None,
            old: Vec::new(),
            old_permit: None,
        }
    }
}
impl Trace for State {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        v(self.image);
        v(self.initializer);
    }
}
impl Trace for Particle {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
#[derive(Clone)]
struct Random(Rc<Cell<Option<u32>>>);
impl Trace for Random {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
fn state<T>(
    cx: &mut NativeCx<'_>,
    owner: ObjId,
    f: impl FnOnce(&mut State) -> NativeResult<T>,
) -> NativeResult<T> {
    extensions::layer_size(cx, Value::Obj(owner.into()))?;
    cx.heap_mut().initialize_native_default::<State>(owner)?;
    cx.heap_mut().with_native_state::<State, _>(owner, f)?
}
fn manager(s: &mut State) -> NativeResult<&mut Model> {
    s.model
        .as_mut()
        .ok_or(NativeError::Message("particle manager is not initialized"))
}
fn budget(cx: &mut NativeCx<'_>) -> NativeResult<Budget> {
    extensions::image_staging_budget(cx.heap_mut())?.ok_or(NativeError::Message(
        "particle storage budget is unavailable",
    ))
}
fn number(cx: &NativeCx<'_>, args: &[Value], i: usize) -> NativeResult<f64> {
    Ok(value::to_real(cx.heap(), arg(args, i)?)?)
}
fn count(cx: &NativeCx<'_>, v: Value) -> NativeResult<usize> {
    usize::try_from(value::to_integer(cx.heap(), v)? as i32)
        .map_err(|_| NativeError::Message("particle count must be nonnegative"))
}

krkr_engine::native_plugin! {
    pub(crate) Particles{
        names:["layerExParticle.dll","layerExParticle.tpm"],
        link(cx,exports){
            let layer=crate::exports::class(cx,"Layer")?;
            for (name,call) in [
                ("initVectorParticle",Call::Resumable(init::<1>)),("initRotateParticle",Call::Resumable(init::<2>)),
                ("initAccelRotateParticle",Call::Resumable(init::<4>)),("initBlinkParticle",Call::Resumable(init::<3>)),
                ("uninitParticle",Call::Leaf(uninit)),("assignParticle",Call::Resumable(assign)),
                ("setParticleAppearArea",Call::Leaf(area)),("setParticleRotateCenter",Call::Leaf(center)),
            ]{exports.function(cx,layer,name,call)?;}
            exports.captured_function(cx,layer,"updateParticle",Call::Resumable(update),Random(Rc::new(Cell::new(None))),false)?;
            for (name,n) in [("ptVector",1),("ptRotate",2),("ptBlink",3),("ptAccelRotate",4)]{exports.value(cx,layer,name,Value::Int(n))?;}
            for property in PROPERTIES{exports.property(cx,layer,property)?;}
            for &(name,call) in PAIRS{exports.function(cx,layer,name,call)?;}
            Ok(())
        }
    }
}
fn init<const K: i32>(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let start = count(cx, arg(args, 0)?)?;
    let max = count(cx, arg(args, 1)?)?;
    let rate = number(cx, args, 2)?;
    let mut area = [0.; 4];
    for (i, p) in area
        .iter_mut()
        .enumerate()
        .take(if K == 1 || K == 3 { 4 } else { 2 })
    {
        *p = number(cx, args, i + 3)?;
    }
    if state(cx, owner, |s| Ok(s.model.is_some()))? {
        return Ok(NativeStep::Return(Value::Void));
    }
    let model = Model::new(K, start, max, rate, area, &budget(cx)?)?;
    state(cx, owner, |s| {
        s.model = Some(model);
        s.old.clear();
        Ok(())
    })?;
    render::clear(cx, owner)
}
fn uninit(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    state(cx, cx.this(), |s| {
        let mode = s.mode;
        *s = State::default();
        s.mode = mode;
        Ok(Value::Void)
    })
}
fn assign(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let source = object(arg(args, 0)?)?;
    if source == owner {
        return Ok(NativeStep::Return(Value::Void));
    }
    let budget = budget(cx)?;
    let copy = state(cx, source, |s| {
        if s.model.is_none() || s.frames.is_empty() {
            return Ok(None);
        }
        let permit = budget
            .reserve(s.frames.len() * std::mem::size_of::<[i32; 4]>())
            .map_err(error)?;
        Ok(Some(State {
            model: Some(manager(s)?.duplicate(&budget)?),
            image: s.image,
            initializer: s.initializer,
            mode: s.mode,
            frames: s.frames.clone(),
            frame_permit: Some(permit),
            old: Vec::new(),
            old_permit: None,
        }))
    })?;
    let initialized = copy.is_some();
    state(cx, owner, |s| {
        if let Some(copy) = copy {
            *s = copy;
        } else {
            let mode = s.mode;
            *s = State::default();
            s.mode = mode;
        }
        Ok(())
    })?;
    if initialized {
        render::clear(cx, owner)
    } else {
        Ok(NativeStep::Return(Value::Void))
    }
}
fn get<const O: usize>(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    state(cx, cx.this(), |s| Ok(Value::Real(manager(s)?.get(O))))
}
fn set<const O: usize, const R: usize>(
    cx: &mut NativeCx<'_>,
    args: &[Value],
) -> NativeResult<Value> {
    let v = number(cx, args, 0)?;
    state(cx, cx.this(), |s| {
        let m = manager(s)?;
        m.parameters[O / 8] = v;
        if R != 0 {
            m.parameters[R / 8 + 2] = m.get(R) - m.get(R + 8);
        }
        Ok(Value::Void)
    })
}
fn pair<const O: usize>(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let a = number(cx, args, 0)?;
    let b = number(cx, args, 1)?;
    state(cx, cx.this(), |s| {
        manager(s)?.pair(O, a, b);
        Ok(Value::Void)
    })
}
fn area(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let mut p = [0.; 4];
    for (i, v) in p.iter_mut().enumerate() {
        *v = number(cx, args, i)?;
    }
    state(cx, cx.this(), |s| {
        manager(s)?.parameters[0xd8 / 8..0xf8 / 8].copy_from_slice(&p);
        Ok(Value::Void)
    })
}
fn center(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let p = [number(cx, args, 0)?, number(cx, args, 1)?];
    state(cx, cx.this(), |s| {
        manager(s)?.parameters[0xd8 / 8..0xe8 / 8].copy_from_slice(&p);
        Ok(Value::Void)
    })
}
fn special<const P: u8>(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    state(cx, cx.this(), |s| {
        Ok(match P {
            0 => Value::Int(i64::from(s.model.is_some() && !s.frames.is_empty())),
            1 => s.image,
            2 => Value::Int(i64::from(s.mode)),
            3 => s.initializer,
            4 => Value::Int(manager(s)?.start as i64),
            5 => Value::Int(manager(s)?.max as i64),
            6 => Value::Real(manager(s)?.rate),
            7 => Value::Int(manager(s)?.count as i64),
            8 => Value::Int(i64::from(manager(s)?.kind)),
            9 => Value::Int(manager(s)?.get(0x50) as i64),
            10 => Value::Int(manager(s)?.get(0x130) as i64),
            _ => unreachable!(),
        })
    })
}
fn special_set<const P: u8>(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let v = arg(args, 0)?;
    if P == 3 {
        return state(cx, cx.this(), |s| {
            manager(s)?;
            s.initializer = v;
            Ok(Value::Void)
        });
    }
    if P == 5 {
        let max = count(cx, v)?;
        let budget = budget(cx)?;
        return state(cx, cx.this(), |s| {
            manager(s)?.resize(max, &budget)?;
            Ok(Value::Void)
        });
    }
    let n = if P == 6 {
        value::to_real(cx.heap(), v)?
    } else {
        f64::from(value::to_integer(cx.heap(), v)? as i32)
    };
    state(cx, cx.this(), |s| {
        match P {
            2 => s.mode = n as i32,
            6 => manager(s)?.rate = n,
            9 => manager(s)?.parameters[0x50 / 8] = n,
            10 => manager(s)?.parameters[0x130 / 8] = n,
            _ => unreachable!(),
        };
        Ok(Value::Void)
    })
}
const fn property(name: &'static str, get: Call, set: Option<Call>) -> NativeProperty {
    NativeProperty {
        name,
        doc: "layerExParticle parameter",
        hidden: false,
        class_only: false,
        get: Some(get),
        set,
    }
}
macro_rules! ranges{($($name:literal,$offset:expr,$cached:expr;)* )=>{
    static PAIRS:&[(&str,Call)]=&[$((concat!("setParticle",$name),Call::Leaf(pair::<$offset>)),)*];
    static PROPERTIES:&[NativeProperty]=&[
        property("initializedParticle",Call::Leaf(special::<0>),None),property("particleImage",Call::Leaf(special::<1>),Some(Call::Resumable(image))),
        property("operateMode",Call::Leaf(special::<2>),Some(Call::Leaf(special_set::<2>))),property("particleInitializer",Call::Leaf(special::<3>),Some(Call::Leaf(special_set::<3>))),
        property("particleStartCount",Call::Leaf(special::<4>),None),property("particleMaxCount",Call::Leaf(special::<5>),Some(Call::Leaf(special_set::<5>))),
        property("particleGenerateRate",Call::Leaf(special::<6>),Some(Call::Leaf(special_set::<6>))),property("particleCount",Call::Leaf(special::<7>),None),property("particleType",Call::Leaf(special::<8>),None),
        property("rotateReverse",Call::Leaf(special::<9>),Some(Call::Leaf(special_set::<9>))),property("particleRotateReverse",Call::Leaf(special::<10>),Some(Call::Leaf(special_set::<10>))),
        property("particleAppearLeft",Call::Leaf(get::<0xd8>),Some(Call::Leaf(set::<0xd8,0>))),property("particleAppearTop",Call::Leaf(get::<0xe0>),Some(Call::Leaf(set::<0xe0,0>))),
        property("particleAppearWidth",Call::Leaf(get::<0xe8>),Some(Call::Leaf(set::<0xe8,0>))),property("particleAppearHeight",Call::Leaf(get::<0xf0>),Some(Call::Leaf(set::<0xf0,0>))),
        property("particleRotateCenterX",Call::Leaf(get::<0xd8>),Some(Call::Leaf(set::<0xd8,0>))),property("particleRotateCenterY",Call::Leaf(get::<0xe0>),Some(Call::Leaf(set::<0xe0,0>))),
        $(property(concat!("maxParticle",$name),Call::Leaf(get::<$offset>),Some(Call::Leaf(set::<$offset,$cached>))),
          property(concat!("minParticle",$name),Call::Leaf(get::<{$offset+8}>),Some(Call::Leaf(set::<{$offset+8},$cached>))),)*
    ];
}}
ranges! {
    "Angle",0x20,0;"AngleOmega",0x38,0;"Magnify",0x58,0x58;"MagnifyVaridation",0x70,0x70;
    "Opacity",0x88,0x88;"OpacityVaridation",0xa0,0xa0;"Speed",0xf8,0xf8;"Accel",0x110,0x110;
    "VectorAngle",0x128,0x128;"VectorRotate",0x140,0x140;"RotateRadius",0xe8,0xe8;"RotateRadiusVaridation",0x100,0x100;
    "RotateOmega",0x118,0x118;"RotateOmegaMax",0x118,0x118;"RotateOmegaTime",0x138,0x138;
    "BlinkMax",0x88,0x88;"BlinkTime",0xf8,0xf8;"BlinkCount",0x110,0x110;
}

#[derive(tjs_bind::Trace)]
struct Updating {
    owner: ObjId,
    rng: Random,
    tick: i32,
    remaining: usize,
    advanced: bool,
    values: Vec<f64>,
    result: Value,
    blink: Option<Particle>,
}
fn update(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let tick = value::to_integer(cx.heap(), arg(args, 0)?)? as i32;
    let function = cx.function().ok_or(NativeError::This)?;
    let rng = cx
        .heap_mut()
        .with_native_state::<Random, _>(function, |r| r.clone())?;
    if rng.0.get().is_none() {
        rng.0.set(Some(extensions::tick_count(cx)? as u32));
    }
    let remaining = state(cx, owner, |s| {
        let m = manager(s)?;
        Ok(m.start.min(m.max).saturating_sub(m.count))
    })?;
    Updating {
        owner,
        rng,
        tick,
        remaining,
        advanced: false,
        values: Vec::new(),
        result: Value::Void,
        blink: None,
    }
    .next(cx)
}
impl Updating {
    fn next(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        loop {
            if self.remaining == 0 {
                if self.advanced {
                    return render::start(cx, self.owner);
                }
                self.remaining = state(cx, self.owner, |s| Ok(manager(s)?.advance(self.tick)))?;
                self.advanced = true;
                continue;
            }
            let hook = state(cx, self.owner, |s| Ok(s.initializer))?;
            if let Value::Obj(mut callable) = hook
                && callable.object.is_some()
            {
                self.blink = state(cx, self.owner, |s| {
                    let m = manager(s)?;
                    Ok(if m.kind == 3 {
                        let mut r = self.rng.0.get().expect("initialized random stream");
                        let p = m.seed_common(&mut r);
                        self.rng.0.set(Some(r));
                        Some(p)
                    } else {
                        None
                    })
                })?;
                callable.this = Some(self.owner);
                return Ok(NativeStep::Call {
                    function: Value::Obj(callable),
                    arguments: Vec::new(),
                    continuation: flow::callback(self, |mut s, cx, v| {
                        s.result = v;
                        s.values.clear();
                        s.read(cx)
                    }),
                });
            }
            let mut r = self.rng.0.get().expect("initialized random stream");
            state(cx, self.owner, |s| {
                let images = s.frames.len();
                let m = manager(s)?;
                for _ in 0..self.remaining {
                    if !m.spawn(&mut r, images, None, None) {
                        break;
                    }
                }
                Ok(())
            })?;
            self.rng.0.set(Some(r));
            self.remaining = 0;
        }
    }
    fn read(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let n = state(cx, self.owner, |s| {
            Ok(match manager(s)?.kind {
                3 => 5,
                4 => 8,
                _ => 7,
            })
        })?;
        if self.values.len() == n {
            let mut r = self.rng.0.get().expect("initialized random stream");
            state(cx, self.owner, |s| {
                let images = s.frames.len();
                manager(s)?.spawn(&mut r, images, Some(&self.values), self.blink.take());
                Ok(())
            })?;
            self.rng.0.set(Some(r));
            self.remaining -= 1;
            self.result = Value::Void;
            return self.next(cx);
        }
        if !matches!(self.result,Value::Obj(o) if o.object.is_some()) {
            self.values.resize(n, 0.);
            return self.read(cx);
        }
        Ok(NativeStep::GetRequiredOr {
            object: self.result,
            key: Value::Int(self.values.len() as i64),
            fallback: Value::Int(0),
            continuation: flow::callback(self, |mut s, cx, v| {
                s.values.push(value::to_real(cx.heap(), v)?);
                s.read(cx)
            }),
        })
    }
}

#[derive(tjs_bind::Trace)]
struct Atlas {
    owner: ObjId,
    image: Value,
    value: Value,
    row: Value,
    index: usize,
    count: usize,
    rects: Vec<[i32; 4]>,
    cells: Vec<i32>,
    #[trace(skip = "Allocation accounting contains no script values")]
    permit: Option<Permit>,
}
fn field<S: Trace + 'static>(
    cx: &mut NativeCx<'_>,
    object: Value,
    key: &str,
    fallback: Value,
    s: S,
    next: fn(S, &mut NativeCx<'_>, Value) -> NativeResult<NativeStep>,
) -> NativeResult<NativeStep> {
    let key = Value::Str(
        cx.heap_mut()
            .alloc_string(key.encode_utf16().collect::<Vec<_>>()),
    );
    Ok(NativeStep::GetRequiredOr {
        object,
        key,
        fallback,
        continuation: flow::callback(s, next),
    })
}
fn image(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let image = arg(args, 0)?;
    extensions::layer_size(cx, image)?;
    state(cx, owner, |s| {
        manager(s)?;
        Ok(())
    })?;
    let s = Atlas {
        owner,
        image,
        value: Value::Void,
        row: Value::Void,
        index: 0,
        count: 0,
        rects: Vec::new(),
        cells: Vec::new(),
        permit: None,
    };
    field(cx, image, "tag", Value::Void, s, |s, cx, v| {
        if matches!(v, Value::Void) {
            field(cx, s.image, "orgTag", Value::Void, s, atlas_tag)
        } else {
            atlas_tag(s, cx, v)
        }
    })
}
fn atlas_tag(s: Atlas, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
    if !matches!(v,Value::Obj(o) if o.object.is_some()) {
        return Err(NativeError::Message(
            "particleImage requires tag or orgTag metadata",
        ));
    }
    field(cx, v, "divideArea", Value::Void, s, |mut s, cx, v| {
        s.value = v;
        match v {
            Value::Str(text) => {
                let text = String::from_utf16_lossy(cx.heap().string(text)?);
                s.reserve(cx, text.split('/').count())?;
                for part in text.split('/') {
                    let mut numbers = part
                        .split(|c: char| !c.is_ascii_digit())
                        .filter(|x| !x.is_empty());
                    let mut cells = [0; 4];
                    for cell in &mut cells {
                        *cell = numbers
                            .next()
                            .ok_or(NativeError::Message(
                                "divideArea needs four coordinates per frame",
                            ))?
                            .parse::<i32>()
                            .map_err(error)?;
                    }
                    if numbers.next().is_some() {
                        return Err(NativeError::Message(
                            "divideArea needs four coordinates per frame",
                        ));
                    }
                    s.rects.push(cells);
                }
                s.finish(cx)
            }
            Value::Obj(_) => field(cx, v, "count", Value::Int(0), s, |mut s, cx, v| {
                s.count = count(cx, v)?;
                s.reserve(cx, s.count)?;
                s.rows(cx)
            }),
            _ => Err(NativeError::Message(
                "particleImage.divideArea must be an array or string",
            )),
        }
    })
}
impl Atlas {
    fn reserve(&mut self, cx: &mut NativeCx<'_>, count: usize) -> NativeResult<()> {
        let bytes = count
            .checked_mul(std::mem::size_of::<[i32; 4]>())
            .ok_or(NativeError::Message("particle atlas size overflow"))?;
        let permit = budget(cx)?.reserve(bytes).map_err(error)?;
        self.rects.try_reserve_exact(count).map_err(error)?;
        self.permit = Some(permit);
        Ok(())
    }
    fn rows(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index == self.count {
            return self.finish(cx);
        }
        Ok(NativeStep::GetRequiredOr {
            object: self.value,
            key: Value::Int(self.index as i64),
            fallback: Value::Void,
            continuation: flow::callback(self, |mut s, cx, v| {
                s.row = v;
                s.cells.clear();
                if matches!(v,Value::Obj(o) if o.object.is_some()) {
                    s.cell(cx)
                } else {
                    s.index += 1;
                    s.rows(cx)
                }
            }),
        })
    }
    fn cell(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.cells.len() == 4 {
            self.rects
                .push([self.cells[0], self.cells[1], self.cells[2], self.cells[3]]);
            self.index += 1;
            return self.rows(cx);
        }
        Ok(NativeStep::GetRequiredOr {
            object: self.row,
            key: Value::Int(self.cells.len() as i64),
            fallback: Value::Int(0),
            continuation: flow::callback(self, |mut s, cx, v| {
                s.cells.push(value::to_integer(cx.heap(), v)? as i32);
                s.cell(cx)
            }),
        })
    }
    fn finish(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        state(cx, self.owner, |s| {
            s.image = self.image;
            s.frames = self.rects;
            s.frame_permit = self.permit;
            Ok(())
        })?;
        Ok(NativeStep::Return(Value::Void))
    }
}
