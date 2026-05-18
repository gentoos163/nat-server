fn main() {
    #[cfg(feature = "gui")]
    slint_build::compile("ui/main.slint").expect("Slint UI 编译失败");
}
