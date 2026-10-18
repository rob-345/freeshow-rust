fn main() {
    println!("cargo:rerun-if-changed=native/shim.c");
    println!("cargo:rerun-if-changed=vendor/ltc.h");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
    {
        cc::Build::new()
            .file("native/shim.c")
            .include("vendor")
            .flag("-std=gnu11")
            .warnings(true)
            .compile("freeshow_ltc_shim");
        println!("cargo:rustc-link-lib=dl");
    }
}
