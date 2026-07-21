fn main() {
    // 用 Material Design 风格编译界面
    let cfg = slint_build::CompilerConfiguration::new().with_style("material".to_string());
    slint_build::compile_with_config("ui/app.slint", cfg).unwrap();
}
