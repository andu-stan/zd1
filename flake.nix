{
  description = "zd1, new firmware for the B&W Zeppelin Air: the firmware builds (nix/), toolchains and dev shells (the emulator is Renode via emu/zemu)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
    }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      # what the builds identify themselves as: the commit (-dirty with uncommitted changes) and its date, UTC
      version = self.shortRev or self.dirtyShortRev or "dev";
      built =
        let
          d = self.lastModifiedDate or "19700101000000";
          s = nixpkgs.lib.substring;
        in
        "${s 0 4 d}-${s 4 2 d}-${s 6 2 d} ${s 8 2 d}:${s 10 2 d}";
      perSystem = nixpkgs.lib.genAttrs systems (
        system:
        import ./nix {
          inherit
            nixpkgs
            rust-overlay
            crane
            system
            version
            built
            ;
        }
      );
      # The module's image and every part of it are built on Linux, also when a Mac asks for them (through a Linux
      # builder): the bytes depend on the build machine (a Mac's C compiler builds the codecs and the compat layer
      # differently), and one platform gives one image for one source, whichever machine builds it, so the rescue's
      # content ID (module/os/assemble.py) means its code changed. Builds on either are reproducible: two of the same
      # source are byte-identical. (`nix develop .#cross` stays native: a Mac builds the module there with its own.)
      linuxOf = {
        aarch64-darwin = "aarch64-linux";
        x86_64-darwin = "x86_64-linux";
      };
      moduleImage = [
        "os-image"
        "module-os-parts"
        "boot"
        "busybox"
        "busybox-rescue"
        "alsa-lib"
        "alsa-utils"
        "wpa_supplicant"
        "warp0-fw"
        "apps"
        "apps-core"
        "apps-debug"
        "debug-tools"
      ];
      packages = nixpkgs.lib.mapAttrs (
        system: s:
        s.packages
        // nixpkgs.lib.optionalAttrs (linuxOf ? ${system}) (
          nixpkgs.lib.getAttrs moduleImage perSystem.${linuxOf.${system}}.packages
        )
      ) perSystem;
    in
    {
      # nix/default.nix. Variants through override, e.g. packages.SYSTEM.boot.override { tag; }
      inherit packages;
      devShells = nixpkgs.lib.mapAttrs (_: s: s.devShells) perSystem;
      formatter = nixpkgs.lib.mapAttrs (_: s: s.formatter) perSystem;
      # the module's clippy runs where its image builds
      checks = nixpkgs.lib.mapAttrs (
        system: s:
        s.checks
        // nixpkgs.lib.optionalAttrs (linuxOf ? ${system}) { inherit (perSystem.${linuxOf.${system}}.checks) clippy-module; }
      ) perSystem;
    };
}
