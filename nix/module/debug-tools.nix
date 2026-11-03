# module/os/debug: the probes (each file's header says what it is for), static. $out/bin; $out/lib/segv.o is linked
# into a program being debugged instead.
{ stdenv, src }:
stdenv.mkDerivation {
  name = "zd1-debug-tools";
  src = src [ "module/os/debug" ];
  buildPhase = ''
    mkdir -p $out/bin $out/lib
    for c in module/os/debug/*.c; do
      case $c in
        */segv.c) $CC -O2 -Wall -c -o $out/lib/segv.o $c ;;
        *) $CC -O2 -Wall -o $out/bin/$(basename $c .c) $c ;;
      esac
    done
  '';
  dontInstall = true;
  stripAllList = [ "bin" ];
}
