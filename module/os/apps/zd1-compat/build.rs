// The shims (src/compat.c).
fn main() {
    cc::Build::new().file("src/compat.c").flag("-O2").compile("zd1compat");
    println!("cargo:rerun-if-changed=src/compat.c");
}
