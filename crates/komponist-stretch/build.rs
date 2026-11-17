use std::path::PathBuf;

fn main() {
    let vendor = PathBuf::from("../../vendor/signalsmith-stretch");
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-changed={}", vendor.display());
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("native/stretch.cpp")
        .include(vendor)
        // Do not inherit Cargo's unoptimized dev/test settings for the DSP implementation.
        .opt_level(3)
        .debug(false)
        .define("NDEBUG", None)
        .warnings(true)
        .extra_warnings(true);
    if build.get_compiler().is_like_msvc() {
        build.flag("/EHsc").flag("/fp:precise");
    } else {
        // Keep IEEE behavior and avoid host-specific instructions / platform FFT dependencies.
        build.flag("-fexceptions").flag("-ffp-contract=off");
    }
    build.compile("komponist_stretch");
}
