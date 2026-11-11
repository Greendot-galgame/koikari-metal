fn main() {
    println!("cargo:rerun-if-changed=native/text.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/text.m")
            .flag("-fobjc-arc")
            .flag("-mmacosx-version-min=12.0")
            .compile("koikari_native_text");
        for framework in ["AppKit", "CoreText", "CoreGraphics", "Foundation"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
