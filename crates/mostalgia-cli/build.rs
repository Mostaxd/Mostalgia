fn main() {
    // sherpa-onnx (speaker diarization) ships shared libraries that the
    // build copies next to the binary; look for them there.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
}
