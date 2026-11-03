# Formatting and the linters that work file by file, for every language in the repo: `nix fmt` (formats, applies the
# linters' fixes, reports the rest), `nix flake check` (fails on a file `nix fmt` would change or a linter finding).
# The tools read their settings from the repo's root (rustfmt.toml, ruff.toml, .clang-format, biome.json, taplo.toml),
# so editors agree. Clippy needs the crates built: the clippy checks (nix/default.nix).
{
  lib,
  treefmt,
  writeShellScript,
  rust,
  ruff,
  clang-tools,
  cppcheck,
  nixfmt,
  statix,
  deadnix,
  shfmt,
  shellcheck,
  biome,
  taplo,
  google-java-format,
  csharpier,
}:
let
  # what isn't ours to format: the vendored Atmel ASF and part headers, the decompiler's C (re/), the generators' output
  skip = {
    vendored = [
      "mcu/asf/**"
      "mcu/dfp-include/**"
      "re/**"
    ];
    generated = [
      "mcu/src/zd1host_gen.h"
      "mcu/src/zd1link_gen.h"
      "mcu/test/zd1host_gen_test.c"
      "mcu/test/zd1link_gen_test.c"
      "mcu/src/dsp_data.c"
      "module/codecs/*/*_tables.c"
      "emu/renode/avr32/*.g.cs"
    ];
  };
  # statix fixes one file per run
  statix-fix = writeShellScript "statix-fix" ''for f in "$@"; do ${lib.getExe statix} fix "$f"; done'';
in
treefmt.withConfig {
  settings = {
    on-unmatched = "debug";
    global.excludes = [
      "downloads/**"
      "blobs/**"
      "*.lock"
    ];

    formatter = {
      rustfmt = {
        command = "${rust}/bin/rustfmt";
        includes = [ "*.rs" ];
      };

      ruff-check = {
        command = lib.getExe ruff;
        options = [
          "check"
          "--fix"
        ];
        includes = [ "*.py" ];
        priority = 0;
      };
      ruff-format = {
        command = lib.getExe ruff;
        options = [ "format" ];
        includes = [ "*.py" ];
        priority = 1;
      };

      clang-format = {
        command = "${clang-tools}/bin/clang-format";
        options = [ "-i" ];
        includes = [
          "*.c"
          "*.h"
        ];
        excludes = skip.vendored ++ skip.generated;
        priority = 0;
      };
      cppcheck = {
        command = lib.getExe' cppcheck "cppcheck";
        options = [
          "--quiet"
          "--error-exitcode=1"
          "--enable=warning,portability,performance"
          "--inline-suppr"
          "--language=c"
          # the host's check programs: an allocation that fails ends them anyway
          "--suppress=nullPointerOutOfMemory:*/check/*"
          "--suppress=nullPointerArithmeticOutOfMemory:*/check/*"
          "--suppress=nullPointerOutOfResources:*/check/*"
          "--suppress=memleakOnRealloc:*/check/*"
          "--suppress=leakReturnValNotUsed:*/check/*"
          "--suppress=normalCheckLevelMaxBranches"
          "-Imodule/codecs/dsp" # the decoders' shared helpers and macros (DSP_IMDCT_LONG_PAIRS)
        ];
        includes = [ "*.c" ];
        excludes = skip.vendored ++ skip.generated;
        priority = 1;
      };

      nixfmt = {
        command = lib.getExe nixfmt;
        options = [ "--width=120" ];
        includes = [ "*.nix" ];
        priority = 1;
      };
      statix = {
        command = statix-fix;
        includes = [ "*.nix" ];
        priority = 0;
      };
      deadnix = {
        command = lib.getExe deadnix;
        options = [ "--edit" ];
        includes = [ "*.nix" ];
        priority = 0;
      };

      shfmt = {
        command = lib.getExe shfmt;
        options = [
          "--write"
          "--indent=2"
        ];
        includes = [ "*.sh" ];
        excludes = skip.vendored;
        priority = 0;
      };
      shellcheck = {
        command = lib.getExe shellcheck;
        includes = [ "*.sh" ];
        excludes = skip.vendored;
        priority = 1;
      };

      # the web page and its emulator test
      biome = {
        command = lib.getExe biome;
        options = [
          "check"
          "--write"
          "--no-errors-on-unmatched"
        ];
        includes = [
          "host/web/**/*.js"
          "host/web/**/*.mjs"
          "host/web/**/*.css"
          "host/web/**/*.html"
          "biome.json"
        ];
      };

      taplo = {
        command = lib.getExe taplo;
        options = [ "format" ];
        includes = [ "*.toml" ];
      };

      # Ghidra's scripts (re/ghidra/scripts)
      google-java-format = {
        command = lib.getExe google-java-format;
        options = [
          "--aosp"
          "--replace"
        ];
        includes = [ "*.java" ];
      };

      # the emulator's Renode models (emu/renode)
      csharpier = {
        command = lib.getExe csharpier;
        options = [ "format" ];
        includes = [ "*.cs" ];
        excludes = skip.generated;
      };
    };
  };
}
