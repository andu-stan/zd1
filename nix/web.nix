# The web page (host/web): our updater and configurator over WebUSB. host/core compiled to WebAssembly (`cargo web`,
# .cargo/config.toml), its JavaScript glue from wasm-bindgen (the CLI must be the version Cargo.lock has: built here
# from crates.io), shrunk by wasm-opt, and the page's static files (host/web/www). A directory to serve over HTTPS or
# from localhost (`python3 tools/zeppelin web`).
{
  craneLib,
  workspace,
  buildWasmBindgenCli,
  fetchCrate,
  rustPlatform,
  binaryen,
}:
let
  wasm-bindgen-cli = buildWasmBindgenCli rec {
    src = fetchCrate {
      pname = "wasm-bindgen-cli";
      version = "0.2.129";
      hash = "sha256-pcecKQd7E8Opw6bkFoE569epUi7gh5qpQF1e5PJY6V8=";
    };
    cargoDeps = rustPlatform.fetchCargoVendor {
      inherit src;
      inherit (src) pname version;
      hash = "sha256-vmUrWVU7kPJJxO5qIVeAkwQyWDELO1Z4Z5gitz2kco8=";
    };
  };
in
craneLib.buildPackage {
  pname = "zd1-web";
  version = "0";
  inherit (workspace) src cargoVendorDir;
  cargoArtifacts = null;
  doCheck = false;
  cargoBuildCommand = "cargo web";
  cargoExtraArgs = "--locked";
  nativeBuildInputs = [
    wasm-bindgen-cli
    binaryen
  ];
  installPhaseCommand = ''
    mkdir -p $out/pkg
    wasm-bindgen --target web --no-typescript --out-dir pkg target/wasm32-unknown-unknown/release/zd1_web.wasm
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int -o $out/pkg/zd1_web_bg.wasm pkg/zd1_web_bg.wasm
    cp pkg/zd1_web.js $out/pkg/
    cp -r --no-preserve=mode ${../host/web/www}/. $out/
  '';
  passthru = { inherit wasm-bindgen-cli; };
}
