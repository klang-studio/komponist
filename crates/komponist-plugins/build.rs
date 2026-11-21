fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=KOMPONIST_BUNDLED_CLAP_MANIFEST_SHA256");
    let target = std::env::var("TARGET").expect("Cargo target triple");
    println!("cargo::rustc-env=KOMPONIST_BUNDLED_CLAP_TARGET={target}");
}
