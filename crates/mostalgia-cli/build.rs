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
}
