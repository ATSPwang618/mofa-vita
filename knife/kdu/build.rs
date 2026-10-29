fn main() {
    for source in ["vendor/bc7enc/bc7enc.cpp", "vendor/bc7enc/bc7enc.h", "src/bc7_wrapper.cpp"] {
        println!("cargo:rerun-if-changed={source}");
    }
    cc::Build::new()
        .cpp(true)
        .std("c++11")
        .cpp_link_stdlib(None)
        .flag_if_supported("-fno-exceptions")
        .flag_if_supported("-fno-rtti")
        .warnings(false)
        .include("vendor/bc7enc")
        .file("vendor/bc7enc/bc7enc.cpp")
        .file("src/bc7_wrapper.cpp")
        .compile("knife_bc7enc");
}
