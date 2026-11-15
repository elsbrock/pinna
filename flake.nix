{
  description = "Pinna — an open, local-first voice satellite OS for the Amazon Echo (2nd gen) family";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      # Cross toolchain for the ARM32 kernel (same toolchain family that built
      # the known-good LibreEcho 6.1 kernel).
      armCross = pkgs.pkgsCross.armv7l-hf-multiplatform.buildPackages;
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          # task runner + rust (toolchain pinned by rust-toolchain.toml)
          just
          rustup
          cargo-zigbuild
          zig
          cargo-nextest
          protobuf
          buf

          # image build
          mkosi
          erofs-utils
          e2fsprogs
          dosfstools
          mtools
          dpkg
          cpio
          xz
          zstd
          util-linux
          gptfdisk

          # kernel build — gcc 15, the series that built the known-good
          # LibreEcho 6.1 kernel (gcc 16 is untested against this tree)
          armCross.gcc15
          armCross.binutils
          gnumake
          flex
          bison
          bc
          perl
          openssl
          dtc
          kmod
          rsync

          # device + VM
          android-tools
          qemu

          # tests (aioesphomeapi, onnxruntime parity)
          python3
          uv
        ];

        shellHook = ''
          export CROSS_COMPILE_ARM=armv7l-unknown-linux-gnueabihf-
        '';
      };

      formatter.${system} = pkgs.nixfmt-rfc-style;
    };
}
