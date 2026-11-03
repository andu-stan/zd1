# zd1 (host/cli): the speaker over USB from a terminal, host/core's flows natively (nusb: IOKit on macOS, usbfs on Linux).
{
  lib,
  stdenv,
  craneLib,
  workspace,
  libiconv,
}:
craneLib.buildPackage {
  pname = "zd1";
  version = "0";
  inherit (workspace) src cargoVendorDir;
  cargoArtifacts = null;
  doCheck = false;
  cargoExtraArgs = "--locked -p zd1-cli";
  buildInputs = lib.optional stdenv.isDarwin libiconv;
  meta.mainProgram = "zd1";
}
