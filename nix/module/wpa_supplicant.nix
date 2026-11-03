# wpa_supplicant and wpa_cli (os.toml [wpa]: WEXT, WPA2-PSK, internal crypto), static. $out/bin (/usr/sbin in the rootfs).
{
  stdenv,
  fetchurl,
  os,
  repo,
}:
let
  w = os.wpa;
in
stdenv.mkDerivation {
  pname = "wpa_supplicant";
  inherit (w) version;
  src = fetchurl { inherit (w) url sha256; };
  sourceRoot = "wpa_supplicant-${w.version}/wpa_supplicant";

  configurePhase = "cp ${repo + "/${w.config}"} .config";
  makeFlags = [ "CC=${stdenv.cc.targetPrefix}cc" ];
  buildFlags = [
    "wpa_supplicant"
    "wpa_cli"
  ];
  enableParallelBuilding = true;
  stripAllList = [ "bin" ]; # symbols too: the rootfs is RAM

  installPhase = ''
    runHook preInstall
    install -Dm755 -t $out/bin wpa_supplicant wpa_cli
    runHook postInstall
  '';
}
