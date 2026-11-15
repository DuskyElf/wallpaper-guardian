{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "wallpaper-guardian";
  version = "0.1.0";

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

  meta = {
    description = "Guardian photos-of-the-day wallpaper pool, slideshow and applier";
    homepage = "https://github.com/DuskyElf/wallpaper-guardian";
    mainProgram = "wallpaper-guardian";
    platforms = lib.platforms.linux;
  };
}
