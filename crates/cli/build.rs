fn main() {
    // The window's logo, so Task Manager and Explorer show it here too.
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=app.rc");
        println!("cargo:rerun-if-changed=../gui/assets/icon.ico");
        embed_resource::compile("app.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
