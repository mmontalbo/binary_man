{
  description = "bman — observation-driven behavioral specification for CLI binaries";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.05";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, nixpkgs-unstable, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config = { allowUnfree = true; };
        };
        pkgsUnstable = import nixpkgs-unstable {
          inherit system;
          config = { allowUnfree = true; };
        };
        rustPkgs = pkgsUnstable;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = (with pkgs; [
            rustPkgs.rustc
            rustPkgs.cargo
            rustPkgs.rustfmt
            rustPkgs.clippy
            rustPkgs.rust-analyzer
            ripgrep
            jq
            coreutils
          ]) ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
            pkgs.darwin.apple_sdk.frameworks.Security
          ];
          shellHook = ''
            export RUST_BACKTRACE=1
          '';
        };
      });
}
