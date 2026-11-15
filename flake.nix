{
  description = "Guardian photos-of-the-day wallpaper pool and slideshow";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      eachSystem = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: nixpkgs.legacyPackages.${system};
    in
    {
      packages = eachSystem (system: {
        default = (pkgsFor system).callPackage ./nix/package.nix { };
        wallpaper-guardian = self.packages.${system}.default;
      });

      # Home-manager only: no NixOS module required. Import this module and
      # set services.wallpaper-guardian.enable.
      homeManagerModules.default = ./nix/hm-module.nix;

      checks = eachSystem (system: {
        default = self.packages.${system}.default;
      });

      overlays.default = final: _prev: {
        wallpaper-guardian = final.callPackage ./nix/package.nix { };
      };

      devShells = eachSystem (system: {
        default = (pkgsFor system).mkShell {
          packages = with pkgsFor system; [
            cargo
            clippy
            rustc
            rustfmt
          ];
        };
      });

      formatter = eachSystem (system: (pkgsFor system).nixfmt-tree);
    };
}
