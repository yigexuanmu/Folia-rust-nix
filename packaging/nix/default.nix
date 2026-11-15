# Folia-rust: one binary plus the Nebbia stroke data.
#
# Linux runtime libraries: cpal needs alsa, keyring uses the Secret Service over dbus, winit needs
# libxkbcommon, and wgpu finds the vulkan/GL drivers at runtime. The nixpkgs cargo hook also forces
# aws-lc-sys, libsqlite3-sys and zstd-sys to link system libraries (`AWS_LC_SYS_USE_SYSTEM=1` and
# friends), so these are both build inputs and runtime dependencies.
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
  systemd,
  vulkan-loader,
  wayland,
  libGL,
  zstd,
  src,
}:

let
  # Libraries needed by the linker at build time and by the loader at runtime; the wrapper uses the same list.
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

  # Cargo.lock is committed, so dependencies are fixed; test/ is a separate workspace, not a member.
  cargoLock.lockFile = "${src}/Cargo.lock";
  cargoBuildFlags = [
    "--package"
    "folia-app"
  ];
  # The tests live in their own workspace (test/Cargo.toml); this derivation only packages the app.
  doCheck = false;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];

  buildInputs = libraries;

  dontCargoInstall = true;
  installPhase = ''
    runHook preInstall

    # The cargo hook always passes --target, so the binary lands in target/<triple>/release;
    # fall back to target/release.
    binary=target/release/folia
    if [ ! -x "$binary" ]; then
      binary="$(ls -d target/*/release/folia | head -n 1)"
    fi

    # Keep the binary and its data together: Nebbia looks for stroke files in the working
    # directory first, then beside the executable.
    install -Dm755 "$binary" $out/libexec/folia/folia
    install -Dm644 assets/nebbia/nebbia-strokes.bin.gz $out/libexec/folia/assets/nebbia/nebbia-strokes.bin.gz

    # Only the Arphic-licensed stroke data and licence texts are separate files; every other
    # asset is compiled into the binary.
    for file in LICENSE assets/nebbia/NOTICE.txt assets/nebbia/COPYING.txt assets/nebbia/ARPHICPL.TXT; do
      install -Dm644 "$file" "$out/share/doc/folia/$(basename "$file")"
    done

    # Install the desktop entry and icon together so the app appears in launchers.
    # The icon name matches `Icon=` in the .desktop file (folia), and StartupWMClass is folia too:
    # winit writes the basename of argv[0] into WM_CLASS on X11, and the wrapper has the same name.
    install -Dm644 packaging/folia.desktop $out/share/applications/folia.desktop
    install -Dm644 assets/icon.png $out/share/icons/hicolor/512x512/apps/folia.png

    # systemd-creds must be on PATH: when the keyring is unavailable, sessions are sealed with it.
    makeWrapper $out/libexec/folia/folia $out/bin/folia \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath libraries} \
      --prefix PATH : ${lib.makeBinPath [ systemd ]}

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
