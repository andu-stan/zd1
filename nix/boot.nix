# module/boot: the Linux boot stub and the minimal test image, as flat binaries (linuxboot.bin, mini-TAG.bin; mini
# prints TAG at boot). `cargo boot` (.cargo/config.toml) on the workspace: armv5te-none-eabi is tier 3, core is built
# from rust-src.
{
  craneLib,
  llvm,
  workspace,
  tag ? "dev",
}:
craneLib.buildPackage {
  pname = "zd1-boot";
  version = tag;
  inherit (workspace) src cargoVendorDir;
  cargoArtifacts = null;
  doCheck = false;
  cargoBuildCommand = "cargo boot";
  cargoExtraArgs = "--locked";

  ZD1_TAG = tag;
  nativeBuildInputs = [ llvm ];
  installPhaseCommand = ''
    mkdir -p $out
    e=target/armv5te-none-eabi/boot
    llvm-objcopy -O binary $e/linuxboot $out/linuxboot.bin
    llvm-objcopy -O binary $e/mini $out/mini-${tag}.bin
  '';
}
