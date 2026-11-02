{
  description = "Minohomo Linux proxy client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs) lib;
          package = pkgs.rustPlatform.buildRustPackage {
            pname = "minohomo";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml ./Cargo.lock ./build.rs ./src ./defaults ./packaging
                ./README.md ./docs ./LICENSE
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            # Nix builds stay offline and retain the non-bundled core workflow.
            buildNoDefaultFeatures = true;
            checkNoDefaultFeatures = true;
            nativeBuildInputs = [ pkgs.cmake pkgs.pkg-config ];

            # NixOS has no /usr/bin tools. Keep sudo's setuid wrapper intact.
            postPatch = ''
              substituteInPlace src/core.rs src/apply.rs src/tray.rs \
                --replace-fail /usr/bin/systemctl ${pkgs.systemd}/bin/systemctl
              substituteInPlace src/terminal.rs \
                --replace-fail /usr/bin/systemd-run ${pkgs.systemd}/bin/systemd-run
              for tool in install mv chown chmod; do
                substituteInPlace src/core.rs \
                  --replace-fail /usr/bin/$tool ${pkgs.coreutils}/bin/$tool
              done
              substituteInPlace src/main.rs src/core.rs \
                --replace-fail 'Command::new("sudo")' 'Command::new("/run/wrappers/bin/sudo")'
              substituteInPlace src/core.rs \
                --replace-fail /usr/lib/systemd/system/minohomo.service "$out/lib/systemd/system/minohomo.service"
              substituteInPlace packaging/minohomo-tray.service packaging/minohomo-tray-config.service \
                --replace-fail /usr/bin/minohomo "$out/bin/minohomo"
              substituteInPlace packaging/minohomo.service \
                --replace-fail '/bin/kill -HUP $MAINPID' '${pkgs.bash}/bin/bash -c "kill -HUP $MAINPID"'
              substituteInPlace src/apply.rs src/integration_tests.rs \
                --replace-fail '#!/bin/sh' '#!${pkgs.runtimeShell}'
              substituteInPlace src/integration_tests.rs \
                --replace-fail /bin/true ${pkgs.coreutils}/bin/true \
                --replace-fail /bin/false ${pkgs.coreutils}/bin/false
            '';

            checkFlags = [ "--skip" "tui::" ];
            postInstall = ''
              install -Dm644 packaging/minohomo.service "$out/lib/systemd/system/minohomo.service"
              for unit in packaging/minohomo-tray*; do
                install -Dm644 "$unit" "$out/lib/systemd/user/$(basename "$unit")"
              done
              install -Dm644 src/assets/tray.png "$out/share/icons/hicolor/150x150/apps/minohomo.png"
              mkdir -p "$out/share/doc/minohomo"
              cp -r README.md docs defaults "$out/share/doc/minohomo/"
              install -Dm644 LICENSE "$out/share/licenses/minohomo/LICENSE"
            '';

            meta = {
              description = "Linux proxy client with a terminal dashboard, desktop tray, and managed Mihomo core";
              homepage = "https://github.com/alex-karev/minohomo";
              license = lib.licenses.gpl3Only;
              platforms = systems;
              mainProgram = "minohomo";
            };
          };
        in { default = package; minohomo = package; });

      nixosModules.default = { config, lib, pkgs, ... }:
        let
          package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        in {
          options.services.minohomo.enable = lib.mkEnableOption "Minohomo proxy client and core service";

          config = lib.mkIf config.services.minohomo.enable {
            environment.systemPackages = [ package ];
            systemd.packages = [ package ];
            security.polkit.enable = lib.mkDefault true;
            systemd.tmpfiles.rules = [ "d /var/lib/minohomo 0755 root root -" ];
            systemd.services.minohomo = {
              overrideStrategy = "asDropin";
              wantedBy = [ "multi-user.target" ];
              unitConfig.ConditionPathExists = [
                "/var/lib/minohomo/mihomo"
                "/var/lib/minohomo/config.yaml"
              ];
            };
          };
        };
    };
}
