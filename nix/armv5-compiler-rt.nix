# nixpkgs' LLVM cross toolchain for armv5tel-unknown-linux-musleabi (the module's ARM926EJ-S), fixed. Two gaps,
# both in compiler-rt (the builtins clang links instead of libgcc), worth upstreaming:
#  - its CMake knows ARM architectures by LLVM's names (armv5te) and skips nixpkgs' "armv5tel": nothing gets built.
#    It is given the same target under LLVM's name (clang reads both the same).
#  - clang looks for the runtime in the per-target directory (lib/<triple>/libclang_rt.builtins.a,
#    clang_rt.crtbegin.o); compiler-rt installs the old layout (lib/linux/libclang_rt.builtins-armv5te.a). Without
#    crtbegin there, clang falls back to GCC's crtbeginT.o and the link fails. The per-target names are added.
# Only compiler-rt built for this target changes; the host's LLVM stays the cached one.
_final: prev:
let
  triple = "armv5tel-unknown-linux-musleabi";
  llvmName = "armv5te-unknown-linux-musleabi";
  fix =
    drv:
    drv.overrideAttrs (old: {
      cmakeFlags = old.cmakeFlags ++ [
        "-DCMAKE_C_COMPILER_TARGET=${llvmName}"
        "-DCMAKE_ASM_COMPILER_TARGET=${llvmName}"
      ];
      postInstall = (old.postInstall or "") + ''
        mkdir -p $out/lib/${triple}
        ln -s ../linux/libclang_rt.builtins-armv5te.a $out/lib/${triple}/libclang_rt.builtins.a
        ln -s ../linux/clang_rt.crtbegin-armv5te.o $out/lib/${triple}/clang_rt.crtbegin.o
        ln -s ../linux/clang_rt.crtend-armv5te.o $out/lib/${triple}/clang_rt.crtend.o
      '';
    });
in
prev.lib.optionalAttrs (prev.stdenv.hostPlatform.config == triple) {
  llvmPackages_21 = prev.llvmPackages_21.overrideScope (
    _: p: {
      compiler-rt-libc = fix p.compiler-rt-libc;
      compiler-rt-no-libc = fix p.compiler-rt-no-libc;
    }
  );
}
