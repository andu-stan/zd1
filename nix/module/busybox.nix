# busybox, static (os.toml [busybox]; busybox.config = our changes on top of `make defconfig`), with the applets
# of os.toml's `applets` (the system's) or, rescue = true, `rescue_applets` (the rescue initramfs's) and no others.
{
  lib,
  stdenv,
  fetchurl,
  buildPackages,
  python3,
  writeText,
  os,
  repo,
  rescue ? false,
}:
let
  bb = os.busybox;
  applets = writeText "busybox-applets" (lib.concatLines (if rescue then bb.rescue_applets else bb.applets));
in
stdenv.mkDerivation {
  pname = if rescue then "busybox-rescue" else "busybox";
  inherit (bb) version;
  src = fetchurl { inherit (bb) url sha256; };

  depsBuildBuild = [ buildPackages.stdenv.cc ]; # Kbuild's host programs
  nativeBuildInputs = [ python3 ];

  postPatch = "patchShebangs scripts applets";

  configurePhase = ''
    runHook preConfigure
    make $makeFlags defconfig >/dev/null
    python3 ${./kconfig-merge.py} .config ${repo + "/${bb.config}"} --applets . ${applets}
    # every remaining question answered with its default (`yes` ends on SIGPIPE: not a failure)
    { yes "" || true; } | make $makeFlags oldconfig >/dev/null
    runHook postConfigure
  '';

  makeFlags = [
    "CROSS_COMPILE=${stdenv.cc.targetPrefix}"
    "CONFIG_CROSS_COMPILER_PREFIX=${stdenv.cc.targetPrefix}"
    "CC=${stdenv.cc.targetPrefix}cc"
    "HOSTCC=cc"
  ];
  enableParallelBuilding = true;
  stripAllList = [ "bin" ]; # symbols too: the rootfs is RAM

  installPhase = ''
    runHook preInstall
    make $makeFlags CONFIG_PREFIX=$out install >/dev/null
    runHook postInstall
  '';
}
