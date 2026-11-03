// ../flacdec.c with FLAC's own limits (8 channels, 65535-sample blocks): Warp0's image keeps 2 and 4608.
fn main() {
    cc::Build::new()
        .file("../flacdec.c")
        .include("..")
        .include("../../dsp")
        .define("FLAC_MAX_CH", "8")
        .define("FLAC_MAX_BLOCK", "65535")
        .warnings(false)
        .compile("zd1flac");
    for f in ["../flacdec.c", "../flacdec.h", "../../dsp/fixed.h"] {
        println!("cargo:rerun-if-changed={f}");
    }
}
