//! Fixed linked-program uniforms. Name literals can fold to slots at the call
//! site; dynamic batch names use bounded dispatch without string hashing.
use std::ops::Index;

macro_rules! uniforms {
    ($($key:ident => $name:literal),+ $(,)?) => {
        enum Key { $($key,)+ Count }
        pub(crate) const NAMES: [&str; Key::Count as usize] = [$($name,)+];
        pub(crate) struct Uniforms<T> { values: [T; Key::Count as usize] }
        impl<T> Uniforms<T> {
            pub fn new(f: impl FnMut(&'static str) -> T) -> Self {
                Self { values: NAMES.map(f) }
            }
        }
        impl<T> Index<&str> for Uniforms<T> {
            type Output = T;
            #[inline]
            fn index(&self, name: &str) -> &T {
                let key = match name {
                    $($name => Key::$key,)+
                    _ => panic!("unknown GLES uniform: {name}"),
                };
                &self.values[key as usize]
            }
        }
    }
}
uniforms! {
    Area0 => "u_area0",
    Area1 => "u_area1",
    Axis => "u_axis",
    Backdrop => "u_backdrop",
    BackdropBounds => "u_backdrop_bounds",
    BackdropColor => "u_backdrop_color",
    BackdropOrigin => "u_backdrop_origin",
    BackdropScale => "u_backdrop_scale",
    BackdropSize => "u_backdrop_size",
    BatchClip0 => "u_batch_clip0",
    BatchClip1 => "u_batch_clip1",
    BatchClip2 => "u_batch_clip2",
    BatchClip3 => "u_batch_clip3",
    BatchMap0 => "u_batch_map0",
    BatchMap1 => "u_batch_map1",
    BatchMap2 => "u_batch_map2",
    BatchMap3 => "u_batch_map3",
    BatchOpacity => "u_batch_opacity",
    BatchSharpen => "u_batch_sharpen",
    BatchScale0 => "u_batch_scale0",
    BatchScale1 => "u_batch_scale1",
    BatchScale2 => "u_batch_scale2",
    BatchScale3 => "u_batch_scale3",
    BatchSize0 => "u_batch_size0",
    BatchSize1 => "u_batch_size1",
    BatchSize2 => "u_batch_size2",
    BatchSize3 => "u_batch_size3",
    BatchSource0 => "u_batch_source0",
    BatchSource1 => "u_batch_source1",
    BatchSource2 => "u_batch_source2",
    BatchSource3 => "u_batch_source3",
    Canvas => "u_canvas",
    Channel => "u_channel",
    Color => "u_color",
    Color2 => "u_color2",
    Curve => "u_curve",
    Data0 => "u_data0",
    Data1 => "u_data1",
    Data2 => "u_data2",
    Data3 => "u_data3",
    Diagonal => "u_diagonal",
    DiagonalBacking => "u_diagonal_backing",
    DiagonalRect => "u_diagonal_rect",
    Direct => "u_direct",
    Down => "u_down",
    DownBacking => "u_down_backing",
    DownRect => "u_down_rect",
    Extent => "u_extent",
    Flip => "u_flip",
    Frame => "u_frame",
    Kind => "u_kind",
    Lookup => "u_lookup",
    LookupWindow => "u_lookup_window",
    MapW => "u_map_w",
    MapX => "u_map_x",
    MapX2 => "u_map_x2",
    MapY => "u_map_y",
    MapY2 => "u_map_y2",
    Mask => "u_mask",
    MaskSize => "u_mask_size",
    Nv12 => "u_nv12",
    Offset => "u_offset",
    Operation => "u_operation",
    OutputScale => "u_output_scale",
    Patch => "u_patch",
    Previous => "u_previous",
    PreviousSize => "u_previous_size",
    Rectangle => "u_rectangle",
    Region => "u_region",
    ResampleKind => "u_resample_kind",
    Right => "u_right",
    RightBacking => "u_right_backing",
    RightRect => "u_right_rect",
    Rule => "u_rule",
    RuleSize => "u_rule_size",
    SampleBounds => "u_sample_bounds",
    Sampling => "u_sampling",
    Source => "u_source",
    Source2 => "u_source2",
    SourceBacking => "u_source_backing",
    SourceBounds => "u_source_bounds",
    SourceOrigin => "u_source_origin",
    SourceRect => "u_source_rect",
    SourceScale => "u_source_scale",
    SourceSize => "u_source_size",
    SourceSize2 => "u_source_size2",
    SourceVisible => "u_source_visible",
    StreamOrigin => "u_stream_origin",
    TableSize => "u_table_size",
    Target => "u_target",
    WeightsSize => "u_weights_size",
}
