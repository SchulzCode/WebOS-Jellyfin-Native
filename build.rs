fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.starts_with("armv7") {
        // LG's libraries, copied off a TV by scripts/fetch-sysroot.sh
        println!("cargo:rustc-link-search=native={}/sysroot/usr/lib", std::env::var("CARGO_MANIFEST_DIR").unwrap());
        cc::Build::new().file("src/native/smp.c").flag("-O2").flag("-Wall").compile("smpshim");
        println!("cargo:rustc-link-lib=dylib=SDL2");
        println!("cargo:rustc-link-lib=dylib=playerAPIs");
        println!("cargo:rustc-link-lib=dylib=avformat");
        println!("cargo:rustc-link-lib=dylib=avcodec");
        println!("cargo:rustc-link-lib=dylib=avutil");
        println!("cargo:rustc-link-lib=dylib=swresample");
    }
    println!("cargo:rerun-if-changed=src/native/smp.c");
    println!("cargo:rerun-if-env-changed=JFNATIVE_SERVER");
}
