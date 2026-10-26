fn main() {
    // Embed the icon and version info into the Windows executable.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("FileDescription", "NopePDF")
            .set("ProductName", "NopePDF")
            .set("LegalCopyright", "Copyright (c) 2026 socherification, MIT License");
        res.compile().expect("compile Windows resources");
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
