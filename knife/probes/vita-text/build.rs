use std::{env, fs, path::PathBuf};

fn main() {
    let here = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repo = here.join("../..").canonicalize().unwrap();
    for path in [
        repo.join("knife/src/text.rs"),
        repo.join("knife/data/exe_tables_witch.bin"),
        repo.join("knife-render/src/vita.rs"),
        repo.join("knife-render/src/vita_test_backend.rs"),
        repo.join("knife-render/src/vita_tests.rs"),
    ] {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("vita") {
        return;
    }
    let render = repo.join("knife-render/src");
    let original = fs::read_to_string(render.join("vita.rs")).unwrap();
    let mut snapshot = original.clone();
    // Only module resolution and upstream test registration change. The
    // production facade's functional code stays byte-for-byte identical.
    // Its desktop-coupled regression suite is run by the root workspace; this
    // independent harness registers the text-specific tests instead.
    let upstream_tests = "#[cfg(all(test, not(target_os = \"vita\")))]\n#[path = \"vita_tests.rs\"]\nmod tests;";
    let normalized = snapshot.replace("\r\n", "\n");
    assert_eq!(normalized.matches(upstream_tests).count(), 1);
    snapshot = normalized.replace(upstream_tests, "// Upstream facade tests are registered in the root workspace.");
    for name in ["vita_test_backend.rs"] {
        let needle = format!("#[path = \"{name}\"]");
        assert_eq!(snapshot.matches(&needle).count(), 1);
        let absolute = render.join(name).to_string_lossy().replace('\\', "/");
        snapshot = snapshot.replace(&needle, &format!("#[path = {absolute:?}]"));
    }
    snapshot.push_str(r#"

// Probe-only introspection of the strict CPU recorder. No runtime code changes.
#[cfg(test)]
pub use test_backend::{Control as ProbeControl, Event as ProbeEvent,
    EffectShader as ProbeShader, EffectBlend as ProbeBlend,
    TextureFormat as ProbeFormat};
#[cfg(test)]
impl Renderer {
    pub fn probe_setup() -> (Self, ProbeControl) {
        let native = Gxm::new().unwrap();
        let control = native.control.clone();
        let renderer = Self::new_with_gxm(native).unwrap();
        (renderer, control)
    }
    pub fn probe_bytes(&self, tex: &GpuTexture) -> Result<Vec<u8>> {
        self.check_texture(tex)?;
        self.device.call(|g| g.read_texture(&tex.native.id))
    }
    pub fn probe_texture_id(tex: &GpuTexture) -> usize { tex.native.id.0 }
}
"#);
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let source = out.join("vita_facade_snapshot.rs");
    fs::write(&source, snapshot).unwrap();
    let absolute = source.to_string_lossy().replace('\\', "/");
    fs::write(out.join("vita_facade_module.rs"), format!(
        "#[path = {absolute:?}] mod vita_facade;\npub use vita_facade::*;\n"
    )).unwrap();
}
