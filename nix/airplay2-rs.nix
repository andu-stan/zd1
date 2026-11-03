# The AirPlay test sender (github.com/lmcgartland/airplay2-rs, its play_audio example): plays a file to the speaker
# over AirPlay 1 or 2 from the Mac; what the receiver measurements in docs/streaming.md used. `nix run .#airplay2-rs`.
{
  lib,
  craneLib,
  fetchFromGitHub,
  stdenv,
  libiconv,
}:
let
  common = {
    pname = "airplay2-rs";
    version = "0-unstable-2026-10-24";
    src = fetchFromGitHub {
      owner = "lmcgartland";
      repo = "airplay2-rs";
      rev = "f824007798f71605307384829800273a4dbbcee2";
      hash = "sha256-7DuVmmwnQJhARbwAnMpsDQZteEYiRMOF5jW39/ESulA=";
    };
    buildInputs = lib.optional stdenv.isDarwin libiconv;
    doCheck = false;
  };
in
craneLib.buildPackage (
  common
  // {
    cargoArtifacts = craneLib.buildDepsOnly (common // { cargoExtraArgs = "--locked -p airplay-client"; });
    cargoExtraArgs = "--locked -p airplay-client --example play_audio";
    installPhaseCommand = "install -Dm755 target/release/examples/play_audio $out/bin/airplay2-rs";
    meta.mainProgram = "airplay2-rs";
  }
)
