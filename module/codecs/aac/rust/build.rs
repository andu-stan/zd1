fn main() {
    let dsp = std::env::var("DEP_ZD1DSP_INCLUDE").expect("zd1-dsp's include path");
    cc::Build::new()
        .file("../aacdec.c")
        .file("../aac_tables.c")
        .include("..")
        .include(&dsp)
        .warnings(false)
        .compile("zd1aac");
    for f in ["../aacdec.c", "../aacdec.h", "../aac_tables.c", "../aac_tables.h"] {
        println!("cargo:rerun-if-changed={f}");
    }
}
