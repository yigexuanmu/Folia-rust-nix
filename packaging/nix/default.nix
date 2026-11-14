# Folia-rust：一个二进制加 Nebbia 的笔画数据。
#
# 系统库按 Linux 需要给：cpal 要 alsa，keyring 走 secret-service（dbus），winit 要 libxkbcommon，
# wgpu 在运行时找 vulkan/GL 驱动。nixpkgs 的 cargo 钩子还会让 aws-lc-sys、libsqlite3-sys、zstd-sys
# 链系统库（AWS_LC_SYS_USE_SYSTEM=1 等），所以这几样既是构建依赖也是运行时依赖。
{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  alsa-lib,
  aws-lc,
  dbus,
  libxkbcommon,
  sqlite,
  vulkan-loader,
  wayland,
  libGL,
  zstd,
  src,
}:

let
  # 构建时要 -L 到、运行时也要能找到的库：wrapper 的 LD_LIBRARY_PATH 用的是同一份。
  libraries = [
    alsa-lib
    aws-lc
    dbus
    libxkbcommon
    sqlite
    vulkan-loader
    wayland
    libGL
    zstd
  ];
in
rustPlatform.buildRustPackage {
  pname = "folia-rust";
  version = "0.1.2";

  inherit src;

  # Cargo.lock 是提交进仓库的，依赖固定；工作区里的 test/ 不在 members 里，不参与构建。
  cargoLock.lockFile = "${src}/Cargo.lock";
  cargoBuildFlags = [
    "--package"
    "folia-app"
  ];
  # 测试工程是独立的 workspace（test/Cargo.toml），这里只打包，不跑它的用例。
  doCheck = false;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];

  buildInputs = libraries;

  dontCargoInstall = true;
  installPhase = ''
    runHook preInstall

    # 构建钩子总带 --target，产物落在 target/<三元组>/release 下；没有三元组时在 target/release。
    binary=target/release/folia
    if [ ! -x "$binary" ]; then
      binary="$(ls -d target/*/release/folia | head -n 1)"
    fi

    # 二进制与其数据放在同一层：nebbia 的笔画文件先按工作目录找，再找可执行文件旁边。
    install -Dm755 "$binary" $out/libexec/folia/folia
    install -Dm644 assets/nebbia/nebbia-strokes.bin.gz $out/libexec/folia/assets/nebbia/nebbia-strokes.bin.gz

    # 只有 Arphic 授权的笔画数据与许可正文是单独文件，其余资源都编进了二进制。
    for file in LICENSE assets/nebbia/NOTICE.txt assets/nebbia/COPYING.txt assets/nebbia/ARPHICPL.TXT; do
      install -Dm644 "$file" "$out/share/doc/folia/$(basename "$file")"
    done

    # 桌面菜单项：.desktop 与图标一起装，装完就能在应用列表里点到。
    # 图标名与 desktop 里的 Icon= 一致（folia），StartupWMClass 也取 folia——
    # winit 在 X11 上把 argv[0] 的文件名写进 WM_CLASS，wrapper 与真实二进制同名。
    install -Dm644 packaging/folia.desktop $out/share/applications/folia.desktop
    install -Dm644 assets/icon.png $out/share/icons/hicolor/512x512/apps/folia.png

    makeWrapper $out/libexec/folia/folia $out/bin/folia \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath libraries}

    runHook postInstall
  '';

  meta = {
    description = "Lyrics visualisation music player, rewritten in native Rust";
    homepage = "https://github.com/yigexuanmu/Folia-rust-nix";
    license = lib.licenses.agpl3Only;
    mainProgram = "folia";
    platforms = lib.platforms.linux;
  };
}
