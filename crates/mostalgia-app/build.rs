fn main() {
    // sherpa-onnx (speaker diarization) ships shared libraries that the
    // build copies next to the binary; look for them there.
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN"),
        // Next to the binary, and in Mostalgia.app's Contents/Frameworks.
        Ok("macos") => {
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path");
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Frameworks");
        }
        _ => {}
    }
    // The Windows exe gets the app icon as resource 1: Explorer, the Start
    // Menu shortcut, toasts and (through GPUI) the windows show it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = "../../packaging/windows/mostalgia.ico";
        println!("cargo:rerun-if-changed={icon}");
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id(icon, "1");
        res.set("ProductName", "Mostalgia");
        res.set("FileDescription", "Mostalgia");
        if let Err(e) = res.compile() {
            // A build without the resource compiler still works, just
            // without the icon.
            println!("cargo:warning=no icon resource: {e}");
        }
    }
}
