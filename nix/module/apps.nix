# The module's Rust programs (os.toml [apps]) as the multicall, bin/zd1: everything (the system's), core = true: what
# the rescue runs (--no-default-features, docs/module/storage.md), or debug = true: the system's debug build (the debug
# tools too, os.toml [image.debug]). `cargo module -p zd1` (.cargo/config.toml) on the
# workspace; crane builds the dependencies once (buildDepsOnly); the cross setup (target, linker, CC for the cc crate)
# follows from the stdenv.
{
  lib,
  stdenv,
  buildPackages,
  craneLib,
  pkg-config,
  alsa-lib,
  warp0-fw,
  workspace,
  os,
  core ? false,
  debug ? false,
}:
let
  common = {
    pname = "zd1-apps";
    version = "0";
    inherit (workspace) src cargoVendorDir;
    strictDeps = true;
    doCheck = false;
    cargoBuildCommand = "cargo module";
    cargoCheckCommand = "true"; # the dependency build compiles them for the module; nothing to check here
    cargoExtraArgs = "--locked -p ${os.apps.multicall}";

    # build scripts run on the build machine; Rust's std on macOS links libiconv
    depsBuildBuild = lib.optional stdenv.buildPlatform.isDarwin buildPackages.libiconv;
    nativeBuildInputs = [ pkg-config ];
    buildInputs = [ alsa-lib ];
    PKG_CONFIG_ALL_STATIC = "1";
    W0_FW_DIR = warp0-fw; # the Warp0 images zd1-warp0 (module/warp0/rust) embeds
    # the static set's -static (its stdenv appends it to NIX_CFLAGS_LINK) reaches the macOS cc too: the build scripts
    # don't link ("library not found for -lcrt0.o"). rustc links the module's binaries statically by itself (musl,
    # crt-static), so it goes.
    preBuild = "unset NIX_CFLAGS_LINK";
  }
  # build scripts link with nixpkgs' linker, as on the other build machines: on x86_64-linux rustc's default is its own
  # rust-lld, which stops at the module's compiler-rt on the search path where GNU ld skips the ARM archive. With
  # --target, cargo gives build scripts no [target.<host>] rustflags, only its [host] table (nightly's host-config),
  # which then needs the host's linker too.
  // lib.optionalAttrs (stdenv.buildPlatform.system == "x86_64-linux") {
    CARGO_UNSTABLE_HOST_CONFIG = "true";
    CARGO_UNSTABLE_TARGET_APPLIES_TO_HOST = "true";
    CARGO_TARGET_APPLIES_TO_HOST = "false";
    CARGO_HOST_LINKER = "${buildPackages.stdenv.cc}/bin/cc";
    CARGO_HOST_RUSTFLAGS = "-Clinker-features=-lld";
  };
  deps = craneLib.buildDepsOnly common;
in
craneLib.buildPackage (
  common
  // {
    pname =
      if core then
        "zd1-core"
      else if debug then
        "zd1-apps-debug"
      else
        "zd1-apps";
    cargoArtifacts = deps;
    cargoExtraArgs =
      common.cargoExtraArgs + lib.optionalString core " --no-default-features" + lib.optionalString debug " --features debug";
    # clippy on the same build (nix flake check), with every feature: `cargo module` as clippy
    passthru.clippy = craneLib.cargoClippy (
      common
      // {
        cargoArtifacts = deps;
        CARGO_PROFILE = "module";
        cargoExtraArgs =
          common.cargoExtraArgs
          + " --features debug --target armv5te-unknown-linux-musleabi -Zbuild-std=std,panic_abort -Zbuild-std-features=";
        cargoClippyExtraArgs = "--keep-going -- --deny warnings";
      }
    );
  }
)
