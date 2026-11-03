// screencapturekit's Swift bridge needs libswiftCompatibility56. Its build script only looks inside
// a full Xcode install, so also point the linker at the active developer dir (Command Line Tools).
// The bridge also loads the Swift runtime through @rpath, which lives in /usr/lib/swift.
fn main() {
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    let Ok(output) = std::process::Command::new("xcode-select")
        .arg("-p")
        .output()
    else {
        return;
    };
    let developer_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let swift_libs = format!("{developer_dir}/usr/lib/swift/macosx");
    if std::path::Path::new(&swift_libs).exists() {
        println!("cargo:rustc-link-search=native={swift_libs}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
