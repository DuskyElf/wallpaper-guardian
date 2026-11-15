{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.wallpaper-guardian;
  toml = pkgs.formats.toml { };

  exe = lib.getExe cfg.package;
  configArgument = lib.optionalString (
    cfg.configFile != null
  ) " --config ${lib.escapeShellArg (toString cfg.configFile)}";

  path = lib.concatStringsSep ":" (
    [ "${cfg.package}/bin" ]
    ++ cfg.path
    ++ [ "${config.home.profileDirectory}/bin" "/run/current-system/sw/bin" ]
  );

  environment =
    [ "PATH=${path}" ]
    ++ lib.optional (cfg.waylandDisplay != null) "WAYLAND_DISPLAY=${cfg.waylandDisplay}"
    ++ lib.mapAttrsToList (
      name: value: lib.escapeShellArg "${name}=${value}"
    ) cfg.environment;
in
{
  options.services.wallpaper-guardian = {
    enable = lib.mkEnableOption "the Guardian wallpaper pool and slideshow";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalMD "the `wallpaper-guardian` package from this flake";
      description = "Package providing the `wallpaper-guardian` binary.";
    };

    settings = lib.mkOption {
      inherit (toml) type;
      default = { };
      description = ''
        Configuration written to {file}`$XDG_CONFIG_HOME/wallpaper-guardian/config.toml`.
        Every key is optional; see the project README for the full reference.
        Keys match the TOML file exactly, for example
        `settings.source.days = 7` or `settings.apply.command = "swww"`.
      '';
      example = lib.literalExpression ''
        {
          source.days = 7;
          apply.args = [ "img" "--transition-type" "fade" "{file}" ];
        }
      '';
    };

    configFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Use this file instead of generating one from {option}`settings`.
        The file is not copied; every unit points at this path.
      '';
    };

    target = lib.mkOption {
      type = lib.types.str;
      default = "graphical-session.target";
      description = "Systemd user target the services belong to.";
    };

    waylandDisplay = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "wayland-1";
      description = ''
        Value for `WAYLAND_DISPLAY` in every unit. The default inherits the
        systemd user environment, which compositors such as niri populate.
        Pin it when the wallpaper daemon has to start before the socket is
        exported.
      '';
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = {
        AWWW_TRANSITION_FPS = "60";
      };
      description = "Extra environment variables for every unit.";
    };

    path = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        Extra PATH entries for the units, after the package and before the
        home-manager and system profiles.
      '';
    };

    fetch = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Run `fetch` from a systemd timer.";
      };
      onBoot = lib.mkOption {
        type = lib.types.str;
        default = "15min";
        description = "Delay after boot before the first fetch.";
      };
      interval = lib.mkOption {
        type = lib.types.str;
        default = "12h";
        description = "Time between fetches, measured on the monotonic clock.";
      };
    };

    rotate = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Rotate the wallpaper from a systemd timer.";
      };
      onCalendar = lib.mkOption {
        type = lib.types.str;
        default = "*:0/6";
        description = "systemd calendar expression for the rotation timer.";
      };
    };

    startup = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Apply a wallpaper when the graphical session starts.";
      };
      after = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "awww-daemon.service" ];
        description = "Units the startup service waits for.";
      };
    };

    resume = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Apply a wallpaper after resume from suspend.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.configFile == null || cfg.settings == { };
        message = "services.wallpaper-guardian: settings is ignored when configFile is set";
      }
    ];

    home.packages = [ cfg.package ];

    xdg.configFile."wallpaper-guardian/config.toml" = lib.mkIf (cfg.configFile == null) {
      source = toml.generate "wallpaper-guardian-config.toml" cfg.settings;
    };

    systemd.user.services = lib.mkMerge [
      {
        wallpaper-fetch = {
          Unit = {
            Description = "Fetch Guardian photos of the day into the wallpaper pool";
            PartOf = [ cfg.target ];
          };
          Service = {
            Type = "oneshot";
            Environment = environment;
            ExecStart = "${exe}${configArgument} fetch";
          };
        };

        wallpaper-rotate = {
          Unit = {
            Description = "Rotate the Guardian wallpaper";
            PartOf = [ cfg.target ];
          };
          Service = {
            Type = "oneshot";
            Environment = environment;
            ExecStart = "${exe}${configArgument} next";
            # The applier may start a wallpaper daemon. Keep it alive when the
            # oneshot unit exits instead of letting the cgroup take it down.
            KillMode = "process";
          };
        };
      }

      (lib.mkIf cfg.startup.enable {
        wallpaper-startup = {
          Unit = {
            Description = "Apply the initial wallpaper at session start";
            PartOf = [ cfg.target ];
            After = cfg.startup.after;
          };
          Service = {
            Type = "oneshot";
            Environment = environment;
            ExecStart = "${exe}${configArgument} next";
            KillMode = "process";
          };
          Install.WantedBy = [ cfg.target ];
        };
      })

      (lib.mkIf cfg.resume.enable {
        wallpaper-resume = {
          Unit = {
            Description = "Re-apply the wallpaper after suspend";
            # suspend.target is reached on the way out, so this runs on
            # resume. sleep.target is the pre-suspend hook.
            After = [ "suspend.target" ];
          };
          Service = {
            Type = "oneshot";
            Environment = environment;
            ExecStart = "${exe}${configArgument} next";
            KillMode = "process";
          };
          Install.WantedBy = [ "suspend.target" ];
        };
      })
    ];

    systemd.user.timers = lib.mkMerge [
      (lib.mkIf cfg.fetch.enable {
        wallpaper-fetch = {
          Unit = {
            Description = "Guardian wallpaper pool fetch";
            PartOf = [ cfg.target ];
          };
          # OnUnitActiveSec uses the monotonic clock, so suspend time does not
          # count and a fetch fires shortly after wake when the interval
          # passed while asleep. Persistent= would only help at boot.
          Timer = {
            OnBootSec = cfg.fetch.onBoot;
            OnUnitActiveSec = cfg.fetch.interval;
          };
          Install.WantedBy = [ cfg.target ];
        };
      })

      (lib.mkIf cfg.rotate.enable {
        wallpaper-rotate = {
          Unit = {
            Description = "Rotate the Guardian wallpaper";
            PartOf = [ cfg.target ];
          };
          Timer.OnCalendar = cfg.rotate.onCalendar;
          Install.WantedBy = [ cfg.target ];
        };
      })
    ];
  };
}
