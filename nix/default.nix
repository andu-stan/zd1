# Everything the project builds, for one host system. The module pieces cross-compile natively on any host with
# nixpkgs' LLVM toolchain; only the MCU needs Linux (Microchip's AVR32 toolchain is an x86_64 Linux binary).
{
  nixpkgs,
  rust-overlay,
  crane,
  system,
  version,
  built,
}:
let
  overlays = [ rust-overlay.overlays.default ];
  pkgs = import nixpkgs { inherit system overlays; };
  inherit (pkgs) lib;

  # The module's CPU, an ARM926EJ-S: ARMv5TE, soft float, Linux 2.6.24 (too old for glibc), so static musl.
  # clang, lld and compiler-rt (armv5-compiler-rt.nix: two fixes for this target); nothing here is C++, so the stdenv
  # leaves libc++ out.
  pkgsArm = import nixpkgs {
    localSystem = system;
    crossSystem = {
      config = "armv5tel-unknown-linux-musleabi";
      gcc = {
        arch = "armv5te";
        float = "soft";
      };
      useLLVM = true;
      linker = "lld";
      isStatic = true;
    };
    # Every byte of the initramfs is RAM on the module: no hardening flags (the 2.6.24 kernel has no use for them),
    # and segments aligned to the kernel's 4 KB pages (lld's ARM default, 64 KB, pads each binary by ~120 KB)
    config.replaceCrossStdenv =
      { buildPackages, baseStdenv }:
      let
        cc = buildPackages.llvmPackages.clangNoLibcxx.overrideAttrs (old: {
          postFixup = (old.postFixup or "") + "\necho '-z max-page-size=4096' >> $out/nix-support/cc-ldflags\n";
        });
      in
      buildPackages.stdenvAdapters.withDefaultHardeningFlags [ ] (buildPackages.overrideCC baseStdenv cc);
    overlays = overlays ++ [ (import ./armv5-compiler-rt.nix) ];
  };

  # the MCU's toolchain only runs here
  pkgsX86 = import nixpkgs { system = "x86_64-linux"; };

  repo = ../.;
  # a derivation's source: only these paths (relative to the repo's top), so it rebuilds when they change and not
  # otherwise
  src =
    paths:
    lib.fileset.toSource {
      root = repo;
      fileset = lib.fileset.unions (map (p: repo + "/${p}") paths);
    };
  os = lib.importTOML ../module/os/os.toml;

  # LLVM on the host: the Warp cores' MIPS I code (module/warp0/mips.py), objcopy for the bare-metal
  # images, llvm-nm. clang is the unwrapped one: the
  # cc-wrapper adds host flags that mean nothing for those targets.
  llvm = pkgs.symlinkJoin {
    name = "zd1-llvm";
    paths =
      let
        l = pkgs.llvmPackages_21;
      in
      [
        l.llvm
        l.lld
        l.clang-unwrapped
      ];
  };

  # the Warp0 firmware's sources: the runtime and the decoders it builds in (module/warp0/build.py)
  warp0Src = src [
    "module/warp0"
    "module/codecs/aac"
    "module/codecs/dsp"
    "module/codecs/vorbis"
    "module/codecs/flac"
  ];

  # Rust: one toolchain (rust-toolchain.toml) for the one workspace (Cargo.toml). Cargo reads every member, so a build's
  # source is the whole workspace; its dependencies are vendored once, std's too (the module's builds compile std and
  # core from rust-src).
  rust = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;
  workspacePaths = [
    "Cargo.toml"
    "Cargo.lock"
    ".cargo"
    "rust-toolchain.toml"
    "protocol"
    "module/dm870"
    "module/link"
    "module/boot"
    "module/codecs"
    "module/warp0"
    "module/os/apps"
    "tools/zeppelin/lab/ap2send"
    "host"
  ];
  # crane on the workspace's toolchain, for everything built for or run on this machine
  hostCraneLib = (crane.mkLib pkgs).overrideToolchain (_: rust);
  workspace = {
    src = src workspacePaths;
    cargoVendorDir = hostCraneLib.vendorMultipleCargoDeps {
      cargoLockList = [
        ../Cargo.lock
        "${rust.passthru.availableComponents.rust-src}/lib/rustlib/src/rust/library/Cargo.lock"
      ];
    };
  };

  # the module's userspace: callPackage here gets the cross stdenv, and its build tools from the host
  module = lib.makeScope pkgsArm.newScope (self: {
    inherit
      repo
      src
      os
      workspace
      ;
    hostPkgs = pkgs; # for what only runs here (the cross set's own build tools are a separate, uncached set on macOS)
    # the toolchain runs here: the native set's (its rust-src, for std from source, is the same everywhere; the
    # cross set's would build a GCC for the module just to unpack it, its buildPackages link statically)
    craneLib = (crane.mkLib pkgsArm).overrideToolchain (_: rust);
    busybox = self.callPackage ./module/busybox.nix { };
    busybox-rescue = self.busybox.override { rescue = true; };
    alsa-lib = self.callPackage ./module/alsa-lib.nix { };
    alsa-utils = self.callPackage ./module/alsa-utils.nix { };
    wpa_supplicant = self.callPackage ./module/wpa_supplicant.nix { };
    warp0-fw = pkgs.callPackage ./warp0-fw.nix {
      inherit llvm;
      src = warp0Src;
    };
    apps = self.callPackage ./module/apps.nix { };
    apps-core = self.apps.override { core = true; };
    apps-debug = self.apps.override { debug = true; };
    debug-tools = self.callPackage ./module/debug-tools.nix { };
  });

  boot = pkgs.callPackage ./boot.nix {
    inherit llvm workspace;
    craneLib = hostCraneLib;
  };

  # what module/os/assemble.py assembles the images from
  module-os-parts = pkgs.linkFarm "zd1-module-os-parts" {
    inherit boot;
    inherit (module)
      busybox
      busybox-rescue
      alsa-lib
      alsa-utils
      wpa_supplicant
      apps
      apps-core
      apps-debug
      ;
  };
  test-unit = pkgs.callPackage ./test-unit.nix { inherit src; };
  os-image = pkgs.callPackage ./os-image.nix {
    inherit
      src
      test-unit
      version
      built
      ;
    parts = module-os-parts;
  };

  # host tools: the AirPlay test sender; zd1 (the speaker over USB, host/cli) and the web page (host/web), host/core's
  # two front-ends
  airplay2-rs = pkgs.callPackage ./airplay2-rs.nix { craneLib = hostCraneLib; };
  zd1 = pkgs.callPackage ./zd1.nix {
    craneLib = hostCraneLib;
    inherit workspace;
  };
  web = pkgs.callPackage ./web.nix {
    craneLib = hostCraneLib;
    inherit workspace;
  };

  # `nix fmt`, and what `nix flake check` runs: every file formatted and linted (treefmt.nix), clippy on every crate as
  # each is built (warnings fail)
  treefmt = pkgs.callPackage ./treefmt.nix { inherit rust; };
  clippy =
    args:
    hostCraneLib.cargoClippy (
      {
        inherit (workspace) src cargoVendorDir;
        version = "0";
        cargoArtifacts = null;
        strictDeps = true;
        buildInputs = lib.optional pkgs.stdenv.isDarwin pkgs.libiconv;
      }
      // args
      // {
        cargoClippyExtraArgs = "--keep-going ${args.cargoClippyExtraArgs or ""} -- --deny warnings";
      }
    );

  mcu = lib.makeScope pkgsX86.newScope (self: {
    inherit src;
    avr32-toolchain = self.callPackage ./mcu/avr32-toolchain.nix { };
    mcu-raw = self.callPackage ./mcu/firmware.nix { };
    mcu-emu-raw = self.mcu-raw.override {
      name = "zd1emu";
      emulator = true;
    };
    telnet-hook = self.callPackage ./mcu/stock-patch.nix { };
  });
  # The firmware with its identification stamped in (mcu/stamp.py): on the host, so a new commit doesn't need the Linux
  # builder unless the firmware's sources changed.
  stampMcu =
    raw:
    let
      tools = src [
        "mcu/stamp.py"
        "tools/zeppelin/__init__.py"
        "tools/zeppelin/fw/ihex.py"
      ];
    in
    pkgs.runCommand raw.name { nativeBuildInputs = [ pkgs.python3 ]; } ''
      python3 -I ${tools}/mcu/stamp.py --version ${version} --built "${built}" ${raw} $out
    '';
in
{
  packages = {
    inherit
      llvm
      boot
      module-os-parts
      os-image
      test-unit
      airplay2-rs
      zd1
      web
      ;
    inherit (module)
      busybox
      busybox-rescue
      alsa-lib
      alsa-utils
      wpa_supplicant
      warp0-fw
      apps
      apps-core
      apps-debug
      debug-tools
      ;
    inherit (mcu)
      mcu-raw
      mcu-emu-raw
      telnet-hook
      avr32-toolchain
      ;
    mcu = stampMcu mcu.mcu-raw;
    mcu-emu = stampMcu mcu.mcu-emu-raw;
  };

  formatter = treefmt;
  checks = {
    format = treefmt.check repo;
    # no per-unit data (MACs, the home network, unit paths) in the tracked files: nix/privacy.py
    privacy = pkgs.runCommand "zd1-privacy" { } ''
      ${lib.getExe pkgs.python3Minimal} -I ${./privacy.py} ${repo}
      touch $out
    '';
    # the MCU's C for both protocols against the Rust's vectors (mcu/test, written by protocol/gen), and its host-volume
    # mapping (hostvol.h), on the host
    mcu-tests =
      let
        tests = src [
          "mcu/src/zd1link.h"
          "mcu/src/zd1link_gen.h"
          "mcu/src/zd1host_gen.h"
          "mcu/src/hostvol.h"
          "mcu/test"
        ];
      in
      pkgs.runCommandCC "zd1-mcu-tests" { } ''
        for t in zd1link_gen zd1host_gen hostvol; do
          $CC -Wall -Werror -I ${tests}/mcu/src -o $t ${tests}/mcu/test/''${t}_test.c
          ./$t
        done
        touch $out
      '';
    # the crates' tests that run here (the protocols, host/core, zd1, the module's crates but its ARM-only code), and
    # the files protocol/gen writes against the specs (stale: fails). zd1-warp0 embeds the Warp0 images.
    host-tests = hostCraneLib.mkCargoDerivation {
      pname = "zd1-host-tests";
      version = "0";
      src = src (
        workspacePaths
        ++ [
          "mcu/src/zd1link_gen.h"
          "mcu/src/zd1host_gen.h"
          "mcu/test"
          "docs/protocol/registers.md"
          "docs/protocol/host.md"
        ]
      );
      inherit (workspace) cargoVendorDir;
      cargoArtifacts = null;
      strictDeps = true;
      # the module's crates that build here too; those on alsa-lib (alsa-sys) only where it builds, on Linux
      nativeBuildInputs = lib.optional pkgs.stdenv.isLinux pkgs.pkg-config;
      buildInputs = lib.optional pkgs.stdenv.isDarwin pkgs.libiconv ++ lib.optional pkgs.stdenv.isLinux pkgs.alsa-lib;
      buildPhaseCargoCommand = ''
        cargo test --locked -p zd1-spi -p zd1-hostlink -p zd1-link -p zd1-host -p zd1-cli \
          -p dm870 -p zd1-flac -p zd1-mdns -p zd1-net -p zd1-persparam -p zd1-settings -p zd1-sys \
          -p zd1-warp0 --features zd1-warp0/airplay \
          ${lib.optionalString pkgs.stdenv.isLinux "-p zd1-audio -p zd1-airplay -p zd1-snapcast -p zd1-linkd"}
        cargo run --locked -q -p zd1-protogen -- --check
      '';
      installPhaseCommand = "mkdir -p $out";
      W0_FW_DIR = module.warp0-fw;
    };
    clippy-host = clippy {
      pname = "zd1-host";
      cargoClippyExtraArgs = "--all-targets";
    };
    clippy-web = clippy {
      pname = "zd1-web";
      cargoExtraArgs = "--locked --target wasm32-unknown-unknown -p zd1-web";
    };
    clippy-boot = clippy {
      pname = "zd1-boot";
      CARGO_PROFILE = "boot";
      cargoExtraArgs = "--locked --target armv5te-none-eabi -p zd1-boot -Zbuild-std=core,compiler_builtins -Zbuild-std-features=compiler-builtins-mem";
    };
    clippy-module = module.apps.clippy;
  };

  devShells = {
    default = pkgs.mkShell {
      packages = with pkgs; [
        (python3.withPackages (
          ps: with ps; [
            pyserial
            numpy
            sounddevice # tools/zeppelin/lab: the microphone and the speaker's USB audio
            unicorn # module/warp0/tools/emu.py
            cryptography # and its encrypted packets
          ]
        ))
        # zd1 (host/cli) from this checkout, built as needed: the speaker over USB
        (writeShellScriptBin "zd1" ''exec cargo run -q --release --manifest-path "''${ZD1_REPO:?run in nix develop}/Cargo.toml" -p zd1-cli -- "$@"'')
        git
        ffmpeg
        llvm
        openocd # JTAG (tools/zeppelin/jtag)
        vorbis-tools # module/codecs/vorbis/check
        rust # the workspace's toolchain (rust-toolchain.toml)
      ];
      ZD1_LLVM = llvm; # module/warp0/mips.py: the Warp tools
      shellHook = ''export ZD1_REPO="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"'';
    };
    # the module's cross toolchain: $CC (clang for armv5te musl, static) and its linker for `cargo module`
    cross =
      let
        target = pkgsArm.stdenv.hostPlatform.rust;
        cc = "${pkgsArm.stdenv.cc.targetPrefix}cc";
      in
      pkgsArm.mkShell {
        # the workspace's toolchain, from the cross set's build tools: its build scripts link for the host as they should
        nativeBuildInputs = [
          (pkgsArm.buildPackages.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml)
          pkgsArm.buildPackages.pkg-config
        ];
        # what the full build needs besides the core's, as in module/apps.nix: alsa-lib (alsa-sys), the Warp0 images
        # (zd1-warp0), libiconv for the build scripts on macOS (librespot-protocol's)
        buildInputs = [ module.alsa-lib ];
        depsBuildBuild = lib.optional pkgs.stdenv.isDarwin pkgs.libiconv;
        env = {
          "CARGO_TARGET_${target.cargoEnvVarTarget}_LINKER" = cc;
          "CC_${target.cargoEnvVarTarget}" = cc;
          PKG_CONFIG_ALL_STATIC = "1";
          W0_FW_DIR = module.warp0-fw;
        };
      };
    # reverse engineering: Ghidra (re/ghidra/setup.py), where nixpkgs has it working
    re =
      let
        ghidra = (builtins.tryEval pkgs.ghidra.outPath).success;
      in
      pkgs.mkShell {
        packages = lib.optional ghidra pkgs.ghidra;
        shellHook = lib.optionalString ghidra "export GHIDRA_HOME=${pkgs.ghidra}/lib/ghidra";
      };
  };
}
