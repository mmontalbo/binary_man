{
  description = "Binary-Validated Man Pages dev environment";

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
        claudePkg = if pkgs ? claude-code then pkgs.claude-code else pkgsUnstable.claude-code;
        codexPkg = if pkgs ? codex then pkgs.codex else pkgsUnstable.codex;
        rustPkgs = pkgsUnstable;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [ claudePkg codexPkg ] ++ (with pkgs; [
            rustPkgs.rustc
            rustPkgs.cargo
            rustPkgs.rustfmt
            rustPkgs.clippy
            rustPkgs.rust-analyzer
            ripgrep
            jq
            file
            binutils
            man-db
            groff
            coreutils
            bubblewrap
          ]) ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.strace
          ];
          shellHook = ''
            export RUST_BACKTRACE=1
          '';
        };
      });
}
