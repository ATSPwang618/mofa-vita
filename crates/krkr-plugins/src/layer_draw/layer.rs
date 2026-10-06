use super::{appearance, coordinates, geometry, matrix::Matrix, path::Path, path_class, render};
use crate::exports::Exports;
use krkr_engine::{extensions, plugins::Context};
use tjs_bind::RestArgs;
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Trace, Value, value};
pub(super) struct State {
    pub transform: Matrix,
    pub view: Matrix,
    pub smooth: i32,
    pub text_hint: i32,
    pub update: bool,
    pub record: Option<super::image::Image>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            transform: Matrix::default(),
            view: Matrix::default(),
            smooth: 4,
            text_hint: 4,
            update: true,
            record: None,
        }
    }
}
impl Trace for State {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
pub(super) fn state<T>(
    cx: &mut NativeCx<'_>,
    owner: tjs_core::ObjId,
    f: impl FnOnce(&mut State) -> T,
) -> NativeResult<T> {
    extensions::layer_size(cx, Value::Obj(owner.into()))?;
    cx.heap_mut().initialize_native_default::<State>(owner)?;
    cx.heap_mut().with_native_state::<State, _>(owner, f)
}
pub(super) fn paint(
    cx: &mut NativeCx<'_>,
    owner: tjs_core::ObjId,
    app: Value,
    path: Path,
) -> NativeResult<NativeStep> {
    let appearance = appearance::snapshot(cx, app)?;
    let (matrix, antialias, update) = state(cx, owner, |s| {
        if !path.segments.is_empty()
            && let Some(record) = &mut s.record
        {
            // The reference records the complete appearance once per drawInfo.
            for _ in &appearance {
                record.records.push(super::image::Record {
                    path: path.clone(),
                    appearance: appearance.clone(),
                });
            }
            record.matrix = s.transform;
        }
        (
            Matrix::new(Matrix::product(s.transform.elements, s.view.elements)),
            true,
            s.update,
        )
    })?;
    let result = geometry::rectangle(cx, [0.; 4])?;
    render::start(
        cx,
        Value::Obj(owner.into()),
        render::Drawing {
            path,
            appearance,
            matrix,
            antialias,
            update,
        },
        result,
    )
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Kind {
    Path,
    Arc,
    Pie,
    Bezier,
    Beziers,
    Closed,
    Closed2,
    Curve,
    Curve2,
    Curve3,
    Ellipse,
    Line,
    Lines,
    Polygon,
    Rect,
    Rects,
}
fn begin(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    kind: Kind,
    count: usize,
) -> NativeResult<NativeStep> {
    if args.len() < count {
        return Err(NativeError::Missing(count - 1));
    }
    let owner = cx.this();
    extensions::layer_prepare_draw(cx, Value::Obj(owner.into()))?;
    let app = args[0];
    if matches!(kind, Kind::Path) {
        let path = path_class::snapshot(cx, args[1])?;
        return paint(cx, owner, app, path);
    }
    let mut numbers = Vec::new();
    let list = matches!(
        kind,
        Kind::Beziers
            | Kind::Closed
            | Kind::Closed2
            | Kind::Curve
            | Kind::Curve2
            | Kind::Curve3
            | Kind::Lines
            | Kind::Polygon
            | Kind::Rects
    );
    if list {
        for (index, &v) in args[2..count].iter().enumerate() {
            numbers.push(if matches!(kind, Kind::Curve3) && index < 2 {
                f64::from(value::to_integer(cx.heap(), v)? as i32)
            } else {
                value::to_real(cx.heap(), v)?
            });
        }
        let fields: &'static [&'static str] = if matches!(kind, Kind::Rects) {
            &["x", "y", "width", "height"]
        } else {
            &["x", "y"]
        };
        return coordinates::read(
            cx,
            args[1],
            fields,
            ((owner, app), (kind, numbers)),
            |((owner, app), (kind, numbers)), cx, values| {
                let points = values.iter().map(|v| [v[0], v[1]]).collect::<Vec<_>>();
                let mut path = Path::default();
                match kind {
                    Kind::Beziers => path.beziers(&points),
                    Kind::Closed => path.closed_curve(&points, 0.5),
                    Kind::Closed2 => path.closed_curve(&points, numbers[0]),
                    Kind::Curve => path.curve(&points, 0, -1, 0.5),
                    Kind::Curve2 => path.curve(&points, 0, -1, numbers[0]),
                    Kind::Curve3 => {
                        path.curve(&points, numbers[0] as i32, numbers[1] as i32, numbers[2])
                    }
                    Kind::Lines => path.lines(&points),
                    Kind::Polygon => path.polygon(&points),
                    Kind::Rects => path.rectangles(
                        &values
                            .iter()
                            .map(|v| [v[0], v[1], v[2], v[3]])
                            .collect::<Vec<_>>(),
                    ),
                    _ => unreachable!(),
                }
                paint(cx, owner, app, path)
            },
        );
    }
    for &v in &args[1..count] {
        numbers.push(value::to_real(cx.heap(), v)?);
    }
    let n = &numbers;
    let mut path = Path::default();
    match kind {
        Kind::Arc => path.arc([n[0], n[1], n[2], n[3]], n[4], n[5])?,
        Kind::Pie => path.pie([n[0], n[1], n[2], n[3]], n[4], n[5])?,
        Kind::Bezier => path.bezier([[n[0], n[1]], [n[2], n[3]], [n[4], n[5]], [n[6], n[7]]]),
        Kind::Ellipse => path.ellipse([n[0], n[1], n[2], n[3]])?,
        Kind::Line => path.line([n[0], n[1]], [n[2], n[3]]),
        Kind::Rect => path.rectangle([n[0], n[1], n[2], n[3]]),
        _ => unreachable!(),
    }
    paint(cx, owner, app, path)
}
#[tjs_bind::function(resumable = true)]
fn path(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Path, 2)
}
#[tjs_bind::function(resumable = true)]
fn arc(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Arc, 7)
}
#[tjs_bind::function(resumable = true)]
fn pie(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Pie, 7)
}
#[tjs_bind::function(resumable = true)]
fn bezier(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Bezier, 9)
}
#[tjs_bind::function(resumable = true)]
fn beziers(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Beziers, 2)
}
#[tjs_bind::function(resumable = true)]
fn closed(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Closed, 2)
}
#[tjs_bind::function(resumable = true)]
fn closed2(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Closed2, 3)
}
#[tjs_bind::function(resumable = true)]
fn curve(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Curve, 2)
}
#[tjs_bind::function(resumable = true)]
fn curve2(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Curve2, 3)
}
#[tjs_bind::function(resumable = true)]
fn curve3(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Curve3, 5)
}
#[tjs_bind::function(resumable = true)]
fn ellipse(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Ellipse, 5)
}
#[tjs_bind::function(resumable = true)]
fn line(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Line, 5)
}
#[tjs_bind::function(resumable = true)]
fn lines(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Lines, 2)
}
#[tjs_bind::function(resumable = true)]
fn polygon(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Polygon, 2)
}
#[tjs_bind::function(resumable = true)]
fn rect(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Rect, 5)
}
#[tjs_bind::function(resumable = true)]
fn rects(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    begin(cx, args, Kind::Rects, 2)
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum TransformKind {
    Set,
    Reset,
    Rotate,
    Scale,
    Translate,
}
fn transform(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    view: bool,
    kind: TransformKind,
) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let count = match kind {
        TransformKind::Reset => 0,
        TransformKind::Set | TransformKind::Rotate => 1,
        _ => 2,
    };
    if args.len() < count {
        return Err(NativeError::Missing(count - 1));
    }
    extensions::layer_prepare_draw(cx, Value::Obj(owner.into()))?;
    if matches!(kind, TransformKind::Set) {
        if let Some(matrix) = super::matrix_class::native(cx, args[0]) {
            return set_matrix(cx, owner, view, matrix);
        }
        if !matches!(args[0],Value::Obj(r) if r.object.is_some()) {
            return Err(NativeError::Type("a matrix object"));
        }
        return coordinates::one(
            cx,
            args[0],
            &["m11", "m12", "m21", "m22", "dx", "dy"],
            (owner, view),
            |(owner, view), cx, p| {
                let p = &p[0];
                set_matrix(
                    cx,
                    owner,
                    view,
                    Matrix::new([p[0], p[1], p[2], p[3], p[4], p[5]].map(|v| v as f32)),
                )
            },
        );
    }
    let mut matrix = Matrix::default();
    match kind {
        TransformKind::Reset => {}
        TransformKind::Rotate => {
            let radians = value::to_real(cx.heap(), args[0])? as f32;
            let (sn, cs) = radians.sin_cos();
            matrix = Matrix::new([cs, sn, -sn, cs, 0., 0.]);
        }
        TransformKind::Scale => {
            matrix = Matrix::new([
                value::to_real(cx.heap(), args[0])? as f32,
                0.,
                0.,
                value::to_real(cx.heap(), args[1])? as f32,
                0.,
                0.,
            ])
        }
        TransformKind::Translate => {
            matrix = Matrix::new([
                1.,
                0.,
                0.,
                1.,
                value::to_real(cx.heap(), args[0])? as f32,
                value::to_real(cx.heap(), args[1])? as f32,
            ])
        }
        TransformKind::Set => unreachable!(),
    }
    assign_transform(cx, owner, view, matrix)
}
fn set_matrix(
    cx: &mut NativeCx<'_>,
    owner: tjs_core::ObjId,
    view: bool,
    matrix: Matrix,
) -> NativeResult<NativeStep> {
    if view && state(cx, owner, |s| s.view.equals(&matrix))? {
        return Ok(NativeStep::Return(Value::Void));
    }
    assign_transform(cx, owner, view, matrix)
}
fn assign_transform(
    cx: &mut NativeCx<'_>,
    owner: tjs_core::ObjId,
    view: bool,
    matrix: Matrix,
) -> NativeResult<NativeStep> {
    state(cx, owner, |s| {
        if view {
            s.view = matrix;
        } else {
            s.transform = matrix;
        }
    })?;
    if view {
        super::record::redraw(cx, owner, Value::Void)
    } else {
        Ok(NativeStep::Return(Value::Void))
    }
}
#[tjs_bind::function(resumable = true)]
fn set_transform(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, false, TransformKind::Set)
}
#[tjs_bind::function(resumable = true)]
fn reset_transform(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, false, TransformKind::Reset)
}
#[tjs_bind::function(resumable = true)]
fn rotate_transform(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, false, TransformKind::Rotate)
}
#[tjs_bind::function(resumable = true)]
fn scale_transform(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, false, TransformKind::Scale)
}
#[tjs_bind::function(resumable = true)]
fn translate_transform(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, false, TransformKind::Translate)
}
#[tjs_bind::function(resumable = true)]
fn set_view(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, true, TransformKind::Set)
}
#[tjs_bind::function(resumable = true)]
fn reset_view(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, true, TransformKind::Reset)
}
#[tjs_bind::function(resumable = true)]
fn rotate_view(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, true, TransformKind::Rotate)
}
#[tjs_bind::function(resumable = true)]
fn scale_view(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, true, TransformKind::Scale)
}
#[tjs_bind::function(resumable = true)]
fn translate_view(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    transform(cx, args, true, TransformKind::Translate)
}
#[tjs_bind::function]
fn get_update(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
    let v = state(cx, cx.this(), |s| s.update)?;
    Ok(Value::Int(i64::from(v)))
}
#[tjs_bind::function]
fn set_update(cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
    let v = value::to_integer(cx.heap(), v)? != 0;
    state(cx, cx.this(), |s| s.update = v)
}
#[tjs_bind::function]
fn get_smooth(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
    let v = state(cx, cx.this(), |s| s.smooth)?;
    Ok(Value::Int(i64::from(v)))
}
#[tjs_bind::function]
fn set_smooth(cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
    let v = value::to_integer(cx.heap(), v)? as i32;
    state(cx, cx.this(), |s| s.smooth = v)
}
#[tjs_bind::function]
fn get_text_hint(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
    let v = state(cx, cx.this(), |s| s.text_hint)?;
    Ok(Value::Int(i64::from(v)))
}
#[tjs_bind::function]
fn set_text_hint(cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
    let v = value::to_integer(cx.heap(), v)? as i32;
    state(cx, cx.this(), |s| s.text_hint = v)
}

pub(super) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    let layer = crate::exports::class(cx, "Layer")?;
    for (name, call) in [
        ("drawString", super::text::draw_string::CALL),
        ("drawPathString", super::text::draw_path_string::CALL),
        ("measureString", super::text::measure::CALL),
        ("measureStringInternal", super::text::measure::CALL),
        ("drawImage", super::image_draw::simple::CALL),
        ("drawImageRect", super::image_draw::rect::CALL),
        ("drawImageStretch", super::image_draw::stretch::CALL),
        ("drawImageAffine", super::image_draw::affine::CALL),
        ("clear", super::record::clear::CALL),
        ("getRecordImage", super::record::image::CALL),
        ("redrawRecord", super::record::redraw_record::CALL),
        ("saveRecord", super::record::save::CALL),
        ("loadRecord", super::record::load::CALL),
        ("drawPath", path::CALL),
        ("drawArc", arc::CALL),
        ("drawPie", pie::CALL),
        ("drawBezier", bezier::CALL),
        ("drawBeziers", beziers::CALL),
        ("drawClosedCurve", closed::CALL),
        ("drawClosedCurve2", closed2::CALL),
        ("drawCurve", curve::CALL),
        ("drawCurve2", curve2::CALL),
        ("drawCurve3", curve3::CALL),
        ("drawEllipse", ellipse::CALL),
        ("drawLine", line::CALL),
        ("drawLines", lines::CALL),
        ("drawPolygon", polygon::CALL),
        ("drawRectangle", rect::CALL),
        ("drawRectangles", rects::CALL),
        ("setTransform", set_transform::CALL),
        ("resetTransform", reset_transform::CALL),
        ("rotateTransform", rotate_transform::CALL),
        ("scaleTransform", scale_transform::CALL),
        ("translateTransform", translate_transform::CALL),
        ("setViewTransform", set_view::CALL),
        ("resetViewTransform", reset_view::CALL),
        ("rotateViewTransform", rotate_view::CALL),
        ("scaleViewTransform", scale_view::CALL),
        ("translateViewTransform", translate_view::CALL),
    ] {
        exports.function(cx, layer, name, call)?;
    }
    for property in PROPERTIES {
        exports.property(cx, layer, property)?;
    }
    exports.property(cx, layer, &super::record::PROPERTY)?;
    Ok(())
}

static PROPERTIES: &[tjs_core::NativeProperty] = &[
    tjs_core::NativeProperty {
        name: "updateWhenDraw",
        doc: "LayerExDraw drawing settings",
        hidden: false,
        class_only: false,
        get: Some(get_update::CALL),
        set: Some(set_update::CALL),
    },
    tjs_core::NativeProperty {
        name: "smoothingMode",
        doc: "LayerExDraw drawing settings",
        hidden: false,
        class_only: false,
        get: Some(get_smooth::CALL),
        set: Some(set_smooth::CALL),
    },
    tjs_core::NativeProperty {
        name: "textRenderingHint",
        doc: "LayerExDraw drawing settings",
        hidden: false,
        class_only: false,
        get: Some(get_text_hint::CALL),
        set: Some(set_text_hint::CALL),
    },
];
