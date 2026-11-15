{
  description = "Folia-rust — 歌词可视化音乐播放器的原生 Rust 实现";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      # Linux only: Windows and macOS use the GitHub Actions artifacts instead.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      eachSystem = f: lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in
    {
      packages = eachSystem (pkgs: {
        default = pkgs.callPackage ./packaging/nix/default.nix { src = self; };
        folia-rust = pkgs.callPackage ./packaging/nix/default.nix { src = self; };
      });

      # `nix develop` uses the same rustc as the package (nixpkgs 1.99.1, matching
      # rust-toolchain.toml), plus the packaging system libraries, so `cargo build` here and
      # `nix build` produce the same thing.
      devShells = eachSystem (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ (pkgs.callPackage ./packaging/nix/default.nix { src = self; }) ];
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });

      formatter = eachSystem (pkgs: pkgs.nixfmt-rfc-style);

      # With `overlays.default`, `pkgs.folia-rust` refers to this package.
      overlays.default = final: _prev: {
        folia-rust = final.callPackage ./packaging/nix/default.nix { src = self; };
      };
    };
}
