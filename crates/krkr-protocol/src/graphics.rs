//! Pixel operations and snapshots use logical image IDs, never backend textures.
mod draws;
pub use draws::{DRAW_BATCH_CAPACITY, PreparedDraw, PreparedDraws};
use slotmap::new_key_type;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU64, Ordering},
};

new_key_type! { pub struct ImageId; pub struct LayerId; }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}
impl Size {
    pub fn rgba_bytes(self) -> Option<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(4)
    }
    pub fn rect(self) -> Rect {
        Rect {
            left: 0,
            top: 0,
            width: self.width,
            height: self.height,
        }
    }
}

/// Half-open rectangles. Intersections widen the endpoints before adding to
/// avoid wrapping legacy signed coordinates near their range limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}
impl Rect {
    pub fn intersection(self, other: Self) -> Option<Self> {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = (i64::from(self.left) + i64::from(self.width))
            .min(i64::from(other.left) + i64::from(other.width));
        let bottom = (i64::from(self.top) + i64::from(self.height))
            .min(i64::from(other.top) + i64::from(other.height));
        (right > i64::from(left) && bottom > i64::from(top)).then_some(Self {
            left,
            top,
            width: (right - i64::from(left)) as u32,
            height: (bottom - i64::from(top)) as u32,
        })
    }
}

/// Resolved draw face; dfAuto is resolved by the Layer's current type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawFace {
    Opaque,
    Alpha,
    AddAlpha,
    Mask,
    Province,
}

pub use crate::blend::{Blend, BlendOptions};

#[derive(Clone, Copy, Debug)]
pub struct Color {
    pub rectangle: Rect,
    pub color: u32,
    pub opacity: i16,
    pub face: DrawFace,
}

#[derive(Clone, Copy, Debug)]
pub struct Fill {
    pub rectangle: Rect,
    /// Legacy packed AARRGGBB, interpreted according to draw face.
    pub color: u32,
    pub face: DrawFace,
    pub hold_alpha: bool,
}

#[derive(Clone, Debug)]
pub struct ImageRef {
    pub id: ImageId,
    pub lifetime: Arc<ImageLifetime>,
}

/// Revisions advance when a write enters the ordered host queue. Even a failed
/// or cancelled write invalidates an old CPU snapshot; only a matching read can
/// install a replacement. Main and province planes can change independently.
#[derive(Debug, Default)]
pub struct ImageLifetime {
    revisions: [AtomicU64; 2],
}
impl ImageLifetime {
    pub fn revision(&self, province: bool) -> u64 {
        self.revisions[usize::from(province)].load(Ordering::Relaxed)
    }
    fn changed(&self, main: bool, province: bool) {
        for (revision, changed) in self.revisions.iter().zip([main, province]) {
            if changed {
                revision.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Debug)]
pub enum ProvinceOperation {
    /// Allocate a standalone R8 plane for a following upload.
    Reserve,
    Fill(Fill),
    Copy {
        source: ImageRef,
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
    },
}
#[derive(Debug)]
pub enum Adjustment {
    Lines(Arc<crate::lines::Lines>),
    BoxBlur {
        radius: [u32; 2],
        alpha: bool,
    },
    Filter(crate::filter::Filter),
    Gradient {
        bounds: Rect,
        from: u32,
        to: u32,
        vertical: bool,
        blend: bool,
    },
    ColorField {
        size: Size,
        hsv: bool,
        axes: [i32; 3],
        /// HSV constants originate from a signed 64-bit string integer cast to
        /// double; RGB constants have already narrowed to their low byte.
        values: [f64; 3],
    },
    Gamma {
        table: Arc<[[u32; 4]; 256]>,
        additive: bool,
    },
    GrayScale,
    Flip {
        horizontal: bool,
    },
}
#[derive(Debug)]
pub enum Command {
    Sprites {
        image: ImageRef,
        source: ImageRef,
        batch: Arc<crate::sprites::Sprites>,
        clip: Rect,
        options: BlendOptions,
    },
    Scanlines {
        image: ImageRef,
        source: ImageRef,
        rows: Arc<crate::scanlines::Scanlines>,
    },
    Warp {
        image: ImageRef,
        source: ImageRef,
        effect: Arc<crate::warp::Warp>,
    },
    /// Main-plane draws admitted by an exclusive, prepaid host write grant.
    PreparedDraw(PreparedDraws),
    /// Share only the main plane; later writes detach through copy-on-write.
    SnapshotMain {
        image: ImageRef,
        source: ImageRef,
    },
    ComposeScene {
        image: ImageRef,
        size: Size,
        scene: Scene,
    },
    Meshes {
        image: ImageRef,
        size: Size,
        batch: crate::mesh::Batch,
    },
    /// Copy a decoded sample at (0,0), clipped to the destination image without
    /// resize or province changes. Optional right-half blue supplies alpha.
    CopyPixels {
        image: ImageRef,
        pixels: Arc<crate::pixels::Pixels>,
        split_alpha: bool,
        size: Size,
    },
    /// Straight source alpha over an additive-alpha main image; no draw-face
    /// conversion or province write. Clip is expressed in image coordinates.
    Perspective {
        image: ImageRef,
        source: ImageRef,
        mapping: crate::transform::Perspective,
        clip: Rect,
    },
    WrappedCopy {
        image: ImageRef,
        source: ImageRef,
        rectangle: Rect,
        destination: Rect,
        shift: (i32, i32),
        clip: Rect,
    },
    PiledCopy {
        image: ImageRef,
        scene: Scene,
        size: Size,
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
    },
    Adjust {
        image: ImageRef,
        rectangle: Rect,
        operation: Adjustment,
    },
    Text {
        image: ImageRef,
        run: crate::text::Run,
        style: crate::text::Style,
        clip: Rect,
    },
    Transform {
        image: ImageRef,
        source: ImageRef,
        rectangle: Rect,
        transform: crate::transform::Transform,
        sampling: crate::transform::Sampling,
        operation: crate::transform::ImageOperation,
        clip: Rect,
        clear: Option<u32>,
    },
    Color {
        image: ImageRef,
        rectangle: Rect,
        color: u32,
        opacity: i16,
        face: DrawFace,
    },
    Operate {
        image: ImageRef,
        source: ImageRef,
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
        options: BlendOptions,
    },
    EnableImage {
        image: ImageRef,
        source: Option<ImageRef>,
        size: Size,
        color: u32,
    },
    CreateProvince {
        image: ImageRef,
        size: Size,
        operation: ProvinceOperation,
    },
    Assign {
        image: ImageRef,
        source: ImageRef,
    },
    /// Replace the main image with a CPU Bitmap, retaining/resizing province.
    AssignBitmap {
        image: ImageRef,
        source: Option<ImageRef>,
        pixels: Arc<crate::pixels::Pixels>,
    },
    /// Sever logical sharing of one existing plane. Missing planes are inert.
    Independ {
        image: ImageRef,
        province: bool,
        copy: bool,
    },
    /// Reserve a private destination for one complete Upload/UploadScaled.
    /// Its pixels must not be observed before that upload succeeds. Backends
    /// may keep the reserved planes uninitialized to avoid a redundant clear.
    BeginUpload {
        image: ImageRef,
        size: Size,
        main: bool,
        province: bool,
        /// Additional CPU storage for decoding, including codec workspace.
        staging_bytes: usize,
        source: Option<ImageRef>,
    },
    PrepareUpload {
        image: ImageRef,
        size: Size,
        main: bool,
        province: bool,
        /// None reserves a standalone cached image without an existing Layer.
        source: Option<ImageRef>,
    },
    /// Complete immutable asset, without reserving an RGBA destination first.
    /// Unsupported backends may decode it explicitly. No province is retained,
    /// matching PrepareUpload(main=true, province=false) followed by Upload.
    LoadCompressed {
        image: ImageRef,
        texture: Arc<crate::texture::Compressed>,
        logical_size: Size,
    },
    Upload {
        image: ImageRef,
        pixels: Arc<crate::pixels::Pixels>,
    },
    /// Stored pixels use a smaller texture while script operations retain the
    /// logical size. Hosts may sample lazily and materialize on pixel writes.
    UploadScaled {
        image: ImageRef,
        pixels: Arc<crate::pixels::Pixels>,
        logical_size: Size,
    },
    /// Decoded YUV 4:2:0, converted to RGB by the GPU at stored size.
    UploadYuv {
        image: ImageRef,
        pixels: Arc<crate::pixels::Yuv420>,
        logical_size: Size,
    },
    /// Effect movie copy, preserving the destination size and province plane.
    CopyYuv {
        image: ImageRef,
        pixels: Arc<crate::pixels::Yuv420>,
        logical_size: Size,
        split_alpha: bool,
        size: Size,
    },
    /// Replace supplied planes without resizing or discarding other planes.
    PatchPixels {
        image: ImageRef,
        pixels: Arc<crate::pixels::Pixels>,
    },
    /// Replace a contained main-plane rectangle, preserving all other pixels.
    PatchRegion {
        image: ImageRef,
        rectangle: Rect,
        pixels: Arc<crate::pixels::Pixels>,
    },
    Create {
        image: ImageId,
        lifetime: Weak<ImageLifetime>,
        size: Size,
        color: u32,
    },
    Resize {
        image: ImageRef,
        size: Size,
        color: u32,
    },
    Fill {
        image: ImageRef,
        fills: Vec<Fill>,
    },
    Copy {
        image: ImageRef,
        source: ImageRef,
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
        face: DrawFace,
        hold_alpha: bool,
    },
    Pixel {
        image: ImageRef,
        x: i32,
        y: i32,
        province: bool,
    },
    ReadImage {
        image: ImageRef,
    },
    ReadRegion {
        image: ImageRef,
        rectangle: Rect,
    },
    ReadProvince {
        image: ImageRef,
    },
    ReadHitPlane {
        image: ImageRef,
        province: bool,
    },
}
impl Command {
    pub(crate) fn invalidate_snapshots(&self) {
        // Read-only commands never discard a cache. Conservatively dirty alpha
        // for complex blends; pure RGB writes and province writes stay separate.
        let (image, main, province) = match self {
            Self::PreparedDraw(_) => return, // Already invalidated on admission.
            Self::Pixel { .. }
            | Self::ReadImage { .. }
            | Self::ReadRegion { .. }
            | Self::ReadProvince { .. }
            | Self::ReadHitPlane { .. }
            | Self::Create { .. } => return,
            Self::Fill { image, fills } => (
                image,
                fills.iter().any(|f| {
                    f.face != DrawFace::Province && !(f.hold_alpha && f.face == DrawFace::Opaque)
                }),
                fills.iter().any(|f| f.face == DrawFace::Province),
            ),
            Self::Copy {
                image,
                face,
                hold_alpha,
                ..
            } => (
                image,
                *face != DrawFace::Province && !(*hold_alpha && *face == DrawFace::Opaque),
                *face == DrawFace::Province,
            ),
            Self::Upload { image, pixels }
            | Self::UploadScaled { image, pixels, .. }
            | Self::PatchPixels { image, pixels }
            | Self::PatchRegion { image, pixels, .. } => {
                (image, pixels.main.is_some(), pixels.province.is_some())
            }
            Self::Independ { copy: true, .. } => return,
            Self::Independ {
                image, province, ..
            } => (image, !province, *province),
            Self::Adjust {
                image, operation, ..
            } => (
                image,
                !matches!(operation, Adjustment::Gamma { .. } | Adjustment::GrayScale),
                matches!(operation, Adjustment::Flip { .. }),
            ),
            Self::Text { image, style, .. } => (
                image,
                !(style.face == DrawFace::Opaque && style.hold_alpha),
                false,
            ),
            Self::Transform {
                image, operation, ..
            } => (
                image,
                !matches!(
                    operation,
                    crate::transform::ImageOperation::Copy { hold_alpha: true }
                ),
                false,
            ),
            Self::Sprites {
                image,
                options,
                batch,
                ..
            } => (
                image,
                true,
                options.face == DrawFace::Province && !batch.clear.is_empty(),
            ),
            Self::WrappedCopy { image, .. }
            | Self::Scanlines { image, .. }
            | Self::Warp { image, .. }
            | Self::Meshes { image, .. }
            | Self::SnapshotMain { image, .. }
            | Self::ComposeScene { image, .. }
            | Self::CopyPixels { image, .. }
            | Self::CopyYuv { image, .. }
            | Self::UploadYuv { image, .. }
            | Self::Perspective { image, .. }
            | Self::PiledCopy { image, .. }
            | Self::Color { image, .. }
            | Self::Operate { image, .. } => (image, true, false),
            Self::EnableImage { image, .. }
            | Self::CreateProvince { image, .. }
            | Self::Assign { image, .. }
            | Self::AssignBitmap { image, .. }
            | Self::LoadCompressed { image, .. }
            | Self::BeginUpload { image, .. }
            | Self::PrepareUpload { image, .. }
            | Self::Resize { image, .. } => (image, true, true),
        };
        image.lifetime.changed(main, province);
    }
    pub fn payload_bytes(&self) -> usize {
        match self {
            Self::PreparedDraw(batch) => batch.payload_bytes(),
            Self::Meshes { batch, .. } => batch.draws.iter().fold(
                batch
                    .draws
                    .capacity()
                    .saturating_mul(std::mem::size_of::<crate::mesh::Draw>())
                    .saturating_add(
                        batch
                            .order
                            .capacity()
                            .saturating_mul(std::mem::size_of::<usize>()),
                    ),
                |bytes, draw| {
                    bytes.saturating_add(
                        draw.masks
                            .capacity()
                            .saturating_mul(std::mem::size_of::<usize>()),
                    )
                },
            ),
            Self::PiledCopy { scene, .. } | Self::ComposeScene { scene, .. } => scene
                .nodes
                .capacity()
                .saturating_mul(std::mem::size_of::<Node>())
                .saturating_add(
                    scene
                        .transitions
                        .capacity()
                        .saturating_mul(std::mem::size_of::<crate::transition::SceneTransition>()),
                ),
            Self::Adjust {
                operation: Adjustment::Gamma { .. },
                ..
            } => 4096,
            Self::Fill { fills, .. } => {
                fills.capacity().saturating_mul(std::mem::size_of::<Fill>())
            }
            // Text runs and immutable masks already carry their font permits.
            Self::Text { .. } => 0,
            _ => 0,
        }
    }
}

/// Parent indices precede children; sibling order is their order in this array.
/// Image leases retain every referenced allocation while the snapshot is live.
#[derive(Clone, Debug)]
pub struct Node {
    /// Identity of an optional composed-subtree cache. Dropping the token lets
    /// backends reclaim it; no VM object or native image handle crosses threads.
    pub cache: Option<Arc<()>>,
    pub visible: bool,
    pub parent: Option<usize>,
    pub image: Option<ImageRef>,
    /// Background for an opaque layer without a main image; other blends keep
    /// their transparent neutral value (BaseLayer::CopySelfForRect).
    pub neutral_color: u32,
    pub rectangle: Rect,
    pub image_left: i32,
    pub image_top: i32,
    pub blend: Blend,
    pub opacity: u8,
}
#[derive(Debug, Default)]
pub struct Scene {
    pub viewport: crate::viewport::Viewport,
    pub requires_op_seq: u64,
    pub nodes: Vec<Node>,
    pub transitions: Vec<crate::transition::SceneTransition>,
}
