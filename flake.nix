{
  description = "Folia-rust — 歌词可视化音乐播放器的原生 Rust 实现";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      # 只打 Linux：Linux 上是这个 flake 的用途，Windows 与 macOS 走 GitHub Actions 的产物。
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

      # `nix develop`：与打包同一个 rustc（nixpkgs 的 1.99.1，与 rust-toolchain.toml 一致），
      # 外加打包用的系统库，因此在 shell 里 `cargo build` 与 `nix build` 编出来的是同一份东西。
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

      # `overlays.default` 之后 `pkgs.folia-rust` 就是本包。
      overlays.default = final: _prev: {
        folia-rust = final.callPackage ./packaging/nix/default.nix { src = self; };
      };
    };
}
