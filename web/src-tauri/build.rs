fn main() {
    let target = std::env::var("TARGET").unwrap();
    println!("cargo:rustc-env=APP_TARGET={}", target);
    tauri_build::build()
}
