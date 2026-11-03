fn main() {
    let dsp = std::env::var("DEP_ZD1DSP_INCLUDE").expect("zd1-dsp's include path");
    cc::Build::new()
        .file("../vorbisdec.c")
        .file("../vorbis_tables.c")
        .include("..")
        .include(&dsp)
        .warnings(false)
        .compile("zd1vorbis");
    for f in ["../vorbisdec.c", "../vorbisdec.h", "../vorbis_tables.c", "../vorbis_tables.h"] {
        println!("cargo:rerun-if-changed={f}");
    }
}
