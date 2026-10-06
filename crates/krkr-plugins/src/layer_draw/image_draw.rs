use super::{
    geometry, image, layer,
    matrix::Matrix,
    path::Path,
    raster::{Brush, Draw, Drawing},
    render,
};
use tjs_bind::RestArgs;
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Value, value};
#[derive(tjs_bind::Trace)]
struct Render {
    owner: tjs_core::ObjId,
    kind: u8,
    numbers: Vec<f64>,
}
fn begin(cx: &mut NativeCx<'_>, args: &[Value], kind: u8) -> NativeResult<NativeStep> {
    let (count, source) = match kind {
        0 => (3, 2),
        1 => (7, 2),
        2 => (9, 4),
        _ => (12, 0),
    };
    if args.len() < count {
        return Err(NativeError::Missing(count - 1));
    }
    let owner = cx.this();
    krkr_engine::extensions::layer_prepare_draw(cx, Value::Obj(owner.into()))?;
    layer::state(cx, owner, |_| ())?;
    let mut numbers = Vec::new();
    for (i, &v) in args[..count].iter().enumerate() {
        if i == source {
            continue;
        }
        numbers.push(if kind == 3 && i == 5 {
            f64::from(v.truthy(cx.heap())?)
        } else {
            value::to_real(cx.heap(), v)?
        });
    }
    image::resolve(
        cx,
        args[source],
        Box::new(Render {
            owner,
            kind,
            numbers,
        }),
    )
}
impl image::Reply for Render {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        image: Option<image::Image>,
    ) -> NativeResult<NativeStep> {
        let result = geometry::rectangle(cx, [0.; 4])?;
        let Some(image) = image else {
            return Ok(NativeStep::Return(result));
        };
        let n = &self.numbers;
        let (rect, matrix) = match self.kind {
            0 => (
                [
                    0.,
                    0.,
                    f64::from(image.size.width),
                    f64::from(image.size.height),
                ],
                [1., 0., 0., 1., n[0], n[1]],
            ),
            1 => ([n[2], n[3], n[4], n[5]], [1., 0., 0., 1., n[0], n[1]]),
            2 => (
                [n[4], n[5], n[6], n[7]],
                [n[2] / n[6], 0., 0., n[3] / n[7], n[0], n[1]],
            ),
            _ => (
                [n[0], n[1], n[2], n[3]],
                if n[4] != 0. {
                    [n[5], n[6], n[7], n[8], n[9], n[10]]
                } else {
                    [
                        n[7] - n[5],
                        n[8] - n[6],
                        n[9] - n[5],
                        n[10] - n[6],
                        n[5],
                        n[6],
                    ]
                },
            ),
        };
        let mut matrix = Matrix::new(matrix.map(|v| v as f32));
        let (antialias, update) = layer::state(cx, self.owner, |s| (s.smooth != 3, s.update))?;
        let mut drawings = Vec::new();
        if let Some(texture) = image.texture {
            let [left, top, width, height] = rect.map(|v| v as i32);
            if width <= 0 || height <= 0 {
                return Ok(NativeStep::Return(result));
            }
            let mut path = Path::default();
            path.rectangle([0., 0., f64::from(width), f64::from(height)]);
            let brush = Brush::Texture {
                image: texture,
                matrix: Matrix::new([1., 0., 0., 1., -(left as f32), -(top as f32)]),
                plain: true,
            };
            drawings.push(render::Drawing {
                path,
                appearance: vec![Draw {
                    offset: [0.; 2],
                    drawing: Drawing::Fill(brush),
                }],
                matrix,
                antialias,
                update,
            });
        } else {
            let mut source = Matrix::default();
            source.scale(
                rect[2] / f64::from(image.size.width),
                rect[3] / f64::from(image.size.height),
                0,
            );
            source.translate(-rect[0], -rect[1], 0);
            matrix = Matrix::new(Matrix::product(
                Matrix::product(matrix.elements, source.elements),
                image.matrix.elements,
            ));
            for record in image.records {
                drawings.push(render::Drawing {
                    path: record.path,
                    appearance: record.appearance,
                    matrix,
                    antialias,
                    update,
                });
            }
        }
        render::batch(
            cx,
            Value::Obj(self.owner.into()),
            render::Batch {
                drawings,
                clear: None,
                update,
                whole: false,
            },
            result,
        )
    }
}
#[tjs_bind::function(resumable = true)]
pub(super) fn simple(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, 0)
}
#[tjs_bind::function(resumable = true)]
pub(super) fn rect(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, 1)
}
#[tjs_bind::function(resumable = true)]
pub(super) fn stretch(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, 2)
}
#[tjs_bind::function(resumable = true)]
pub(super) fn affine(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, 3)
}
