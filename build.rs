fn main() {
    let config = slint_build::CompilerConfiguration::new().with_library_paths([("toast".to_string(), slint_toast::UI_DIR.into())].into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI compiles");

    // Icon and version info of the .exe (Explorer, taskbar pin, Alt+Tab before the window shows).
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("assets/icon.ico")
        .set("FileDescription", "SurvivorsForge")
        .set("ProductName", "SurvivorsForge")
        .compile()
        .expect("Windows resources compile");
}
