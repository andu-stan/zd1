// Each binary links at its own address: linuxboot.ld (0x61F00000, copied there by the trampoline), mini.ld
// (0x60008000, where the preloader enters).
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    for bin in ["linuxboot", "mini"] {
        println!("cargo:rustc-link-arg-bin={bin}=-T{dir}/{bin}.ld");
        println!("cargo:rerun-if-changed={bin}.ld");
    }
    println!("cargo:rerun-if-env-changed=ZD1_TAG");
}
