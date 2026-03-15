{
  description = "Mirror Software Center dev shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }: let
    system = "x86_64-linux";
    pkgs = nixpkgs.legacyPackages.${system};
  in {
    devShells.${system}.default = pkgs.mkShell {
      name = "mirror-software-center";
      packages = with pkgs; [
        rustc           # Rust compiler (1.91 in nixos-unstable, satisfies rust-version = "1.85")
        cargo
        clippy
        rustfmt
        just            # justfile build runner (just build-release, just run, etc.)
        pkg-config      # library discovery at build time
        openssl         # reqwest TLS
        flatpak         # libflatpak bindings (flatpak feature)
        wayland         # Wayland compositor protocol (libcosmic)
        wayland-protocols # xdg-portal, wl-shell, etc.
        libxkbcommon    # keyboard handling (libcosmic dep)
        vulkan-loader   # wgpu GPU backend
        mesa            # OpenGL/Vulkan ICD for local dev/testing
        dbus            # D-Bus (logind-zbus, notify-rust)
      ];
      env = {
        OPENSSL_NO_VENDOR = "1";
        PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig:${pkgs.flatpak}/lib/pkgconfig";
      };
      shellHook = ''
        echo "Mirror Software Center dev shell — Rust $(rustc --version) ready."
        echo "  Build:  just build-release"
        echo "  Run:    just run"
        echo "  Check:  just check"
      '';
    };
  };
}
