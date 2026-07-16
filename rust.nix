{ pkgs, craneLib, sqlx-cli }:

let
  libDir = builtins.path {
    path = builtins.getEnv "PWD" + "/lib";
    name = "lib";
  };

  depsSrc = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter = path: type:
      let base = builtins.baseNameOf path;
      in type == "directory" || base == "Cargo.toml" || base == "Cargo.lock";
  };

  cargoVendorDir = craneLib.vendorCargoDeps {
    src = ./.;
    cargoLock = ./Cargo.lock;
  };

  # utoipa-swagger-ui's build script downloads the Swagger UI dist zip at
  # compile time, which fails inside the networkless nix build sandbox.
  # Pre-fetch it and hand it over via SWAGGER_UI_DOWNLOAD_URL (file:// path).
  swaggerUiZip = pkgs.fetchurl {
    url =
      "https://github.com/swagger-api/swagger-ui/archive/refs/tags/v5.17.14.zip";
    sha256 = "1p6cf4zf3jrswqa9b7wwgxhp3ca2v5qrzxzfp8gv35r0h78484j8";
  };

  commonArgs = {
    pname = "albion-rest-api";
    version = "0.1.0";
    src = ./.;

    inherit cargoVendorDir;

    nativeBuildInputs = [ sqlx-cli pkgs.pkg-config pkgs.curl ];

    buildInputs = [ pkgs.openssl pkgs.sqlite ]
      ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin
      [ pkgs.apple-sdk_15 ];

    COMMIT_SHA = builtins.getEnv "COMMIT_SHA";

    # reqwest (rustls-native-certs) panics constructing a client when the
    # sandbox has no CA bundle — tests build clients even for local mocks.
    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";

    # Stage a WRITABLE copy of the zip: build.rs fs::copy preserves the source
    # mode, and cargo runs the build script more than once per build — a 0444
    # store path would make the second copy fail with PermissionDenied.
    preConfigure = ''
      cp ${swaggerUiZip} "$TMPDIR/swagger-ui.zip"
      chmod 644 "$TMPDIR/swagger-ui.zip"
      export SWAGGER_UI_DOWNLOAD_URL="file://$TMPDIR/swagger-ui.zip"
    '';

    postUnpack = ''
      rm -rf $sourceRoot/lib
      ln -s ${libDir} $sourceRoot/lib
    '';
  };

  cargoArtifacts = craneLib.buildDepsOnly (commonArgs // { src = depsSrc; });

  sqlxSetup = ''
    set -eo pipefail

    export DATABASE_URL="sqlite:$TMPDIR/build.db"
    sqlx db create
    sqlx migrate run --source migrations
  '';

in {
  package = craneLib.buildPackage (commonArgs // {
    inherit cargoArtifacts;
    preBuild = sqlxSetup;
    doCheck = true;

    meta = {
      description = "Albion REST API server";
      homepage = "https://github.com/albionlabs/albion.rest.api";
    };
  });

  clippy = craneLib.cargoClippy (commonArgs // {
    inherit cargoArtifacts;
    preBuild = sqlxSetup;
    cargoClippyExtraArgs = "--all-targets --all-features -- -D clippy::all";
  });
}
