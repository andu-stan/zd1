fn main() {
    cc::Build::new().file("../imdct.c").file("../imdct_tables.c").include("..").warnings(false).compile("zd1dsp");
    for f in ["../imdct.c", "../imdct.h", "../imdct_tables.c", "../fixed.h"] {
        println!("cargo:rerun-if-changed={f}");
    }
    // for the decoders' build scripts (DEP_ZD1DSP_INCLUDE)
    println!("cargo:include={}", std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").display());
}
