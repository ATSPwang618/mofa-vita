//! Reference author/license notices: reference-notices.txt.
//! Kirikiri2 fftgraph/Main.cpp: 2048-sample DST, logarithmic bands and peak decay.
//! The original permits a single shared spectrum; state is per engine, never static.
use krkr_engine::{
    extensions::{self, GpuImage, ImageContinuation, SampleBuffer},
    protocol::{
        graphics::Size,
        pixels::{Bytes, Pixels},
    },
};
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;
use tjs_bind::{RestArgs, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value, value};
const N: usize = 2048;
krkr_engine::native_plugin! {
    pub(crate) FftGraph {
        names: ["fftgraph.dll","fftgraph.tpm"],
        link(cx,exports) {
            let class=crate::exports::class(cx,"WaveSoundBuffer")?;
            cx.heap.initialize_native_default::<Spectrum>(class)?;
            cx.heap.with_native_state::<Spectrum,_>(class,|s|*s=Spectrum::default())?;
            exports.function(cx,cx.global,"drawFFTGraph",draw::CALL)
        }
    }
}
struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    values: Vec<i32>,
    peaks: Vec<i32>,
    counts: Vec<i32>,
    cut: f32,
}
impl Default for Spectrum {
    fn default() -> Self {
        Self {
            fft: FftPlanner::new().plan_fft_forward(N * 2),
            values: Vec::new(),
            peaks: Vec::new(),
            counts: Vec::new(),
            cut: 0.,
        }
    }
}
impl Trace for Spectrum {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl Spectrum {
    fn transform(&self, samples: &[i16]) -> Vec<f32> {
        let mut data = vec![Complex::new(0., 0.); N * 2];
        for (j, &s) in samples.iter().take(N).enumerate() {
            let v = f32::from(s)
                * (std::f32::consts::PI * (j as f32 + 0.5) / N as f32).sin()
                * (4. / 32768. / N as f32);
            data[j].re = v;
            data[N * 2 - 1 - j].re = -v;
        }
        self.fft.process(&mut data);
        let mut output = vec![0.; N];
        // Ooura ddst(n,-1): a[k]=sum_j x[j] sin(pi*(j+.5)*k/n), k=1..n.
        // a[0] stores k=n. The plugin then explicitly clears bins 0 and 1.
        for k in 2..N {
            let angle = std::f32::consts::PI * k as f32 / (2 * N) as f32;
            output[k] = -0.5 * (data[k].im * angle.cos() - data[k].re * angle.sin());
        }
        output
    }
    fn bands(&mut self, fft: &[f32], count: usize, cut: f32, max: i32, decay: [i32; 3]) {
        let [fall, hold, peakfall] = decay;
        if self.values.len() != count || self.cut != cut {
            self.values = vec![0; count];
            self.peaks = vec![0; count];
            self.counts = vec![0; count];
            self.cut = cut;
        }
        for i in 0..count {
            let start =
                (N as f32).powf(i as f32 * ((cut - 1.) / cut) / count as f32 + 1. / cut) as usize;
            let end = (N as f32).powf((i + 1) as f32 * ((cut - 1.) / cut) / count as f32 + 1. / cut)
                as usize;
            let end = end.max(start + 1).min(N);
            let start = start.min(end);
            let amplitude = fft[start..end].iter().fold(0f32, |a, v| a.max(v.abs()));
            let db = if amplitude == 0. {
                -70.
            } else {
                (10. * (amplitude * amplitude).log10()).clamp(-70., 0.)
            };
            let v = (max as f32 - max as f32 / -70. * db) as i32;
            self.values[i] = (self.values[i] - fall).max(0).max(v).min(max);
            if self.counts[i] == hold {
                self.peaks[i] = (self.peaks[i] - peakfall).max(0);
            } else {
                self.counts[i] += 1;
            }
            if self.peaks[i] < v {
                self.peaks[i] = v;
                self.counts[i] = 0;
            }
        }
    }
}
#[derive(tjs_bind::Trace)]
struct Draw {
    layer: Value,
    sound: Value,
    options: Value,
    class: ObjId,
    rect: [i32; 4],
    values: [i32; 7],
    index: usize,
    #[trace(skip = "Scoped PCM buffer contains no VM values")]
    buffer: SampleBuffer,
}
const KEYS: [&str; 7] = [
    "type",
    "division",
    "thick",
    "oncolor",
    "offcolor",
    "bgcolor",
    "peakcolor",
];
fn key(cx: &mut NativeCx<'_>, s: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(s.encode_utf16().collect::<Vec<_>>()),
    )
}
#[tjs_bind::function(resumable = true)]
fn draw(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    if args.len() < 6 {
        return Err(NativeError::Missing(5));
    }
    extensions::layer_size(cx, args[0])?;
    crate::exports::object(args[1])?;
    let mut rect = [0; 4];
    for (i, v) in rect.iter_mut().enumerate() {
        *v = value::to_integer(cx.heap(), args[i + 2])? as i32;
    }
    if rect[2] <= 0 || rect[3] <= 0 {
        return Ok(NativeStep::Return(Value::Void));
    }
    if rect[2] > 4096 || rect[3] > 4096 || i64::from(rect[2]) * i64::from(rect[3]) > 4_194_304 {
        return Err(NativeError::Message("FFT graph exceeds size limit"));
    }
    let class = cx
        .heap()
        .registered_class("WaveSoundBuffer")
        .ok_or(NativeError::This)?;
    let s = Draw {
        layer: args[0],
        sound: args[1],
        options: args.get(6).copied().unwrap_or(Value::Void),
        class,
        rect,
        values: [
            0,
            16,
            2,
            0xff000000u32 as i32,
            0xffb0b0b0u32 as i32,
            0xffc0c0c0u32 as i32,
            0xff707070u32 as i32,
        ],
        index: 0,
        buffer: extensions::sample_buffer(cx, N)?,
    };
    Ok(NativeStep::CallMember {
        object: s.sound,
        key: key(cx, "getVisBuffer"),
        arguments: vec![
            s.buffer.token(),
            Value::Int(N as i64),
            Value::Int(1),
            Value::Int(0),
        ],
        continuation: flow::callback(s, |s, cx, count| {
            if value::to_integer(cx.heap(), count)? < N as i64 {
                s.buffer.clear();
            }
            s.option(cx)
        }),
    })
}
impl Draw {
    fn option(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index >= 7
            || (self.index > 0 && self.values[0] != 1)
            || matches!(
                self.options,
                Value::Void | Value::Obj(tjs_core::ObjRef { object: None, .. })
            )
        {
            return self.paint(cx);
        }
        let fallback = Value::Int(self.values[self.index].into());
        Ok(NativeStep::GetRequiredOr {
            object: self.options,
            key: key(cx, KEYS[self.index]),
            fallback,
            continuation: flow::callback(self, |mut s, cx, v| {
                s.values[s.index] = value::to_integer(cx.heap(), v)? as i32;
                s.index += 1;
                s.option(cx)
            }),
        })
    }
    fn paint(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.values[0] != 0 && self.values[0] != 1 {
            return Ok(NativeStep::Return(Value::Void));
        }
        let [_, _, width, height] = self.rect;
        let size = Size {
            width: width as u32,
            height: height as u32,
        };
        let [kind, division, thick, _, _, _, _] = self.values;
        if kind == 1
            && (division <= 0
                || division > width
                || thick <= 0
                || thick > height
                || height / thick < 3)
        {
            return Err(NativeError::Message(
                "invalid FFT bar division or thickness",
            ));
        }
        let budget = extensions::layer_pixel_budget(cx, self.layer)?;
        let mut bytes = Bytes::zeroed(
            size.rgba_bytes()
                .ok_or(NativeError::Message("FFT size overflow"))?,
            &budget,
        )
        .map_err(|e| NativeError::Detail(e.to_string()))?;
        cx.heap_mut()
            .with_native_state::<Spectrum, _>(self.class, |s| {
                let fft = self.buffer.read(|pcm| s.transform(pcm));
                let output = bytes.as_mut_slice();
                let mut pixel = |x: i32, y: i32, c: u32| {
                    let i = ((height - 1 - y) * width + x) as usize * 4;
                    output[i..i + 4].copy_from_slice(&[
                        (c >> 16) as u8,
                        (c >> 8) as u8,
                        c as u8,
                        (c >> 24) as u8,
                    ]);
                };
                if kind == 0 {
                    s.bands(
                        &fft,
                        width as usize,
                        3.7,
                        height - 1,
                        [(height + 15) / 16, 30, (height + 31) / 32],
                    );
                    const FIRE: [u32; 16] = [
                        0xff20ff00, 0xff40ff00, 0xff60ff00, 0xff80ff00, 0xffa0ff00, 0xffc0ff00,
                        0xffe0ff00, 0xffffff00, 0xffffe000, 0xffffc000, 0xffffa000, 0xffff8000,
                        0xffff6000, 0xffff4000, 0xffff2000, 0xffffff00,
                    ];
                    for x in 0..width {
                        for y in 0..s.values[x as usize] {
                            let c = 15 - s.values[x as usize] + y;
                            pixel(x, y, if c < 0 { 0xff00ff00 } else { FIRE[c as usize] });
                        }
                        pixel(x, s.peaks[x as usize].clamp(0, height - 1), 0xff808080);
                    }
                } else {
                    let th = 1000 / (height / thick);
                    let bw = width / division;
                    s.bands(&fft, division as usize, 8., 1000 - th * 2, [40, 40, 10]);
                    for band in 0..division {
                        let mut peak = false;
                        let mut y = 0;
                        let mut v = 0;
                        while y + thick <= height {
                            let c = if !peak
                                && s.peaks[band as usize] <= v
                                && self.values[6] != self.values[4]
                            {
                                peak = true;
                                self.values[6]
                            } else if s.values[band as usize] >= v {
                                self.values[3]
                            } else {
                                self.values[4]
                            };
                            for dy in 0..thick {
                                for dx in 0..bw {
                                    pixel(
                                        band * bw + dx,
                                        y + dy,
                                        if dy == 0 || dx == 0 {
                                            self.values[5] as u32
                                        } else {
                                            c as u32
                                        },
                                    );
                                }
                            }
                            y += thick;
                            v += th;
                        }
                    }
                }
            })?;
        // Bar mode intentionally leaves remainder rows/columns untouched, like the reference.
        let size = if kind == 1 {
            Size {
                width: (width / division * division) as u32,
                height: (height / thick * thick) as u32,
            }
        } else {
            size
        };
        let top = self.rect[1].saturating_add(height - size.height as i32);
        let pixels = if size.width != width as u32 || size.height != height as u32 {
            let mut cropped = Bytes::zeroed(size.rgba_bytes().unwrap(), &budget)
                .map_err(|e| NativeError::Detail(e.to_string()))?;
            for y in 0..size.height as usize {
                let start = ((height as usize - size.height as usize + y) * width as usize) * 4;
                let dest = y * size.width as usize * 4;
                let n = size.width as usize * 4;
                cropped.as_mut_slice()[dest..dest + n]
                    .copy_from_slice(&bytes.as_slice()[start..start + n]);
            }
            Pixels {
                size,
                main: Some(cropped),
                province: None,
            }
        } else {
            Pixels {
                size,
                main: Some(bytes),
                province: None,
            }
        };
        let target = extensions::gpu_image(cx, size)?;
        extensions::upload_image(
            cx,
            target,
            Arc::new(pixels),
            Box::new(Copy {
                layer: self.layer,
                x: self.rect[0],
                y: top,
            }),
        )
    }
}
#[derive(tjs_bind::Trace)]
struct Copy {
    layer: Value,
    x: i32,
    y: i32,
}
impl ImageContinuation for Copy {
    fn image(self: Box<Self>, cx: &mut NativeCx<'_>, image: GpuImage) -> NativeResult<NativeStep> {
        extensions::layer_copy_image_region(cx, self.layer, image, self.x, self.y)
    }
}
