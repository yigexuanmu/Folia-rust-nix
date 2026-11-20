# Folia-rust：一个二进制加运行时的资源文件。
#
# 系统库按 Linux 需要给：cpal 要 alsa，keyring 走 secret-service（dbus），winit 要 libxkbcommon，
# wgpu 在运行时找 vulkan/GL 驱动。nixpkgs 的 cargo 钩子还会让 aws-lc-sys、libsqlite3-sys、zstd-sys
# 链系统库（AWS_LC_SYS_USE_SYSTEM=1 等），所以这几样既是构建依赖也是运行时依赖。
{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  makeWrapper,
  unzip,
  alsa-lib,
  aws-lc,
  dbus,
  libxkbcommon,
  sqlite,
  vulkan-loader,
  wayland,
  libGL,
  zstd,
  fetchurl,
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

  # 模型权重和 Steam Audio 运行库不在 Git 里，发布包从清单里固定的地址取；这里用同一份地址与哈希。
  # 地址列表与 assets/models/manifest.toml 的 mirrors 一致，前一个取不到时按顺序尝试。
  fetchModel =
    file: hash:
    fetchurl {
      urls = [
        "https://github.com/chthollyphile/folia-major/releases/download/models-v1/${file}"
        "https://hf-mirror.com/HUAI4236/folia-models/resolve/main/${file}"
        "https://huggingface.co/HUAI4236/folia-models/resolve/main/${file}"
        "https://github.com/AZURE-HUAI/folia-models/releases/download/weights-v1/${file}"
      ];
      sha256 = hash;
    };

  beatThis = fetchModel "beat_this-wgsl.bin" "6365ae5ba2e42922772da01fbfd0bde8dd20b7362a679018e78a5f251fe8a967";
  htdemucs = fetchModel "htdemucs-wgsl.bin" "27d5331a74a4e23b18172b3d59d0378e698fd79bc0d0fed3316da77e3a37d8d1";

  # Steam Audio 4.8.1 只提供 linux-x64 的 libphonon.so；其它 Linux 架构没有这个运行库。
  hasSteamAudio = stdenv.hostPlatform.system == "x86_64-linux";
  steamAudio = fetchurl {
    url = "https://github.com/ValveSoftware/steam-audio/releases/download/v4.8.1/steamaudio_4.8.1.zip";
    sha256 = "4a0aa5ec1176f38f0b0993a37c2259d9e86f27e22d5e24f83ec4c3cb9a1d5449";
  };
in
rustPlatform.buildRustPackage {
  pname = "folia-rust";
  version = "0.1.3";

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
    unzip
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

    # 二进制与其数据放在同一层：程序按可执行文件所在目录找 assets/。
    install -Dm755 "$binary" $out/libexec/folia/folia
    cp -R assets $out/libexec/folia/assets

    # 发布包从清单里取的两份模型权重。
    install -Dm644 ${beatThis} $out/libexec/folia/assets/models/beat_this-wgsl.bin
    install -Dm644 ${htdemucs} $out/libexec/folia/assets/models/htdemucs-wgsl.bin
  ''
  + lib.optionalString hasSteamAudio ''
    # Steam Audio 的 Linux x64 运行库。
    mkdir -p $out/libexec/folia/assets/acoustics
    unzip -p ${steamAudio} steamaudio/lib/linux-x64/libphonon.so > $out/libexec/folia/assets/acoustics/libphonon.so
    chmod 755 $out/libexec/folia/assets/acoustics/libphonon.so
  ''
  + ''
    # 许可正文单独放到文档目录，方便发行版和用户查看。
    for file in LICENSE assets/nebbia/NOTICE.txt assets/nebbia/COPYING.txt assets/nebbia/ARPHICPL.TXT \
      assets/nebbia/nebbiaRaindrop.LICENSE.txt assets/paper-shaders/LICENSE assets/paper-shaders/NOTICE \
      assets/cinema/NOTICE assets/cinema/THREE-LICENSE.txt assets/preview/NOTICE.txt \
      assets/netease-api/LICENSE assets/netease-api/NOTICE; do
      install -Dm644 "$file" "$out/share/doc/folia/$(basename "$file")"
    done
    install -Dm644 README.md $out/share/doc/folia/README.md

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
