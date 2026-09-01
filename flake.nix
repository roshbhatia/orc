{
  description = "Effect-based control plane for agent harnesses";

  nixConfig = {
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    systems.url = "github:nix-systems/default";
    bun2nix.url = "github:nix-community/bun2nix/2.1.2";
  };

  outputs =
    inputs:
    let
      eachSystem = inputs.nixpkgs.lib.genAttrs (import inputs.systems);
      pkgsFor = eachSystem (
        system:
        import inputs.nixpkgs {
          inherit system;
          overlays = [ inputs.bun2nix.overlays.default ];
        }
      );
    in
    {
      formatter = eachSystem (
        system:
        let
          pkgs = pkgsFor.${system};
        in
        pkgs.writeShellApplication {
          name = "orc-format";
          runtimeInputs = [
            pkgs.fd
            pkgs.nixfmt
          ];
          text = ''
            if [ "$#" -gt 0 ] && [ "''${1#-}" = "$1" ]; then
              exec nixfmt "$@"
            fi
            exec fd --extension nix --type file --exec-batch nixfmt "$@"
          '';
        }
      );

      packages = eachSystem (system: {
        default = pkgsFor.${system}.callPackage ./package.nix { };
      });

      apps = eachSystem (system: {
        default = {
          type = "app";
          program = "${inputs.nixpkgs.lib.getExe inputs.self.packages.${system}.default}";
        };
      });

      checks = eachSystem (system: {
        default = pkgsFor.${system}.callPackage ./package.nix { };
      });

      devShells = eachSystem (system: {
        default = pkgsFor.${system}.mkShellNoCC {
          packages =
            with pkgsFor.${system};
            [
              biome
              bun
              ffmpeg
              fish
              jq
              ripgrep
              shellcheck
              shfmt
              vhs
            ]
            ++ [ inputs.bun2nix.packages.${system}.default ];
          shellHook = ''
            export BUN2NIX_NATIVE=${inputs.nixpkgs.lib.getExe inputs.bun2nix.packages.${system}.default}
          '';
        };
      });
    };
}
