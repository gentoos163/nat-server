fn main() {
    #[cfg(feature = "gui")]
    slint_build::compile("ui/main.slint").expect("Slint UI 编译失败");

    // Windows：将 ICO 图标嵌入 exe（资源管理器、任务栏、标题栏显示）
    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icons/logo.ico");
        res.compile().unwrap_or_else(|e| {
            eprintln!("winresource warning: {e}");
        });
    }
}
