{
  description = "Binary-Validated Man Pages dev environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            ripgrep
            jq
            file
            binutils
            man-db
            groff
            coreutils
          ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            strace
          ];
          shellHook = ''
            export RUST_BACKTRACE=1
          '';
        };
      });
}
