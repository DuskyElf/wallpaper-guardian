{
  lib,
  rustPlatform,
  cacert,
}:

let
  cargo = builtins.fromTOML (builtins.readFile ../Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = cargo.package.name;
  version = cargo.package.version;

  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      let
        name = baseNameOf (toString path);
      in
      name != "target" && name != "result" && lib.cleanSourceFilter path type;
  };

  cargoLock.lockFile = ../Cargo.lock;

  # The test suite only speaks HTTP, but reqwest builds its TLS verifier when
  # the client is created, and the sandbox has no system certificate store.
  nativeCheckInputs = [ cacert ];
  preCheck = ''
    export SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt
  '';

  meta = {
    inherit (cargo.package) description;
    homepage = "https://github.com/DuskyElf/wallpaper-guardian";
    mainProgram = "wallpaper-guardian";
    platforms = lib.platforms.linux;
  };
}
