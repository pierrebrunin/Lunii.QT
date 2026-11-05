fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/main.slint", config).unwrap();
    println!("cargo:rerun-if-changed=lang");

    // Windows: application icon and version details in the executable
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(windows)]
        {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("../../res/lunii.ico")
                .set("ProductName", "Lunii.QT")
                .set(
                    "FileDescription",
                    "Lunii.QT - Lunii and Flam storytellers manager",
                );
            res.compile().expect("Windows resources");
        }
        println!("cargo:rerun-if-changed=../../res/lunii.ico");
    }
}
