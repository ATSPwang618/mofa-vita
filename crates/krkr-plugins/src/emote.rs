//! Shared E-mote data and animation implementation for Motion and D3D players.
//! Motion and motionPlayer share the same resource and animation provider.
mod adaptor;
mod container;
pub(crate) mod device_player;
mod drawing;
mod eye;
mod initialize;
mod manager;
pub(crate) mod mesh;
mod metadata;
mod model;
mod motion_object;
mod offscreen;
mod pixels;
mod playback;
mod player;
mod query;
pub(crate) mod render;
mod resource;
mod sample;
mod scene;
mod state;
mod timeline;
mod transform;

krkr_engine::native_plugin! {
    pub(crate) Emote {
        names: ["emoteplayer.dll", "emoteplayer.tpm", "motionplayer.dll", "motionplayer.tpm"],
        link(cx, exports) {
            let motion = namespace::install(cx.heap)?;
            cx.heap.initialize_class_state::<namespace::State>(motion)?;
            for (name, class) in [
                ("ResourceManager", manager::bindings::install(cx.heap)?),
                ("EmotePlayer", player::bindings::install(cx.heap)?),
                ("Player", cx.heap.register_class(&player::PLAYER)?),
                ("SeparateLayerAdaptor", adaptor::bindings::install(cx.heap)?),
                ("D3DAdaptor", offscreen::bindings::install(cx.heap)?),
            ] { cx.export_member(motion, name, tjs_core::Value::Obj(class.into()))?; }
            for (name, value) in [("MaskModeAlpha", 1), ("PlayFlagForce", 1)] {
                cx.export_member(motion, name, tjs_core::Value::Int(value))?;
            }
            let player = player::bindings::install(cx.heap)?;
            for (name, value) in [("TimelinePlayFlagParallel", 0), ("TimelinePlayFlagDifference", 1)] {
                cx.export_member(player, name, tjs_core::Value::Int(value))?;
            }
            exports.value(cx, cx.global, "Motion", tjs_core::Value::Obj(motion.into()))
        }
    }
}
#[tjs_bind::class(name = "Motion", static_class = true)]
mod namespace {
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        enabled: bool,
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method(name = "getD3DAvailable")]
        fn available() -> bool {
            true
        }
        #[tjs::getter(name = "enableD3D", class_only = true)]
        fn enabled(&self) -> bool {
            self.enabled
        }
        #[tjs::setter(name = "enableD3D", class_only = true)]
        fn set_enabled(&mut self, value: bool) {
            self.enabled = value;
        }
    }
}
