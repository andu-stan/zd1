# alsa-lib, static (os.toml [alsa]): the apps link it (alsa crate), its config goes to /usr/share/alsa in the rootfs.
# $out/share/alsa: that config, without UCM and topology (nothing here uses them).
{
  stdenv,
  fetchurl,
  os,
}:
let
  a = os.alsa;
in
stdenv.mkDerivation {
  pname = "alsa-lib";
  inherit (a) version;
  src = fetchurl { inherit (a.lib) url sha256; };

  configureFlags = [
    "--with-configdir=/usr/share/alsa" # where it looks at run time, on the module
    "--with-plugindir=/usr/lib/alsa-lib"
    "--disable-python"
    "--disable-topology"
    "--disable-alisp"
    "--disable-old-symbols"
    "--without-debug"
  ];
  env.CFLAGS = "-Os";
  enableParallelBuilding = true;

  installFlags = [ "alsaconfigdir=${placeholder "out"}/share/alsa" ];
  postInstall = "rm -rf $out/share/alsa/ucm* $out/share/alsa/topology";
}
