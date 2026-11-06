{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  wrapGAppsHook4,
  glib,
  gtk4,
  libadwaita,
  webkitgtk_6_0,
  gettext,
  appstream,
  glib-networking,
  gsettings-desktop-schemas,
  adwaita-icon-theme,
  shared-mime-info,
  gnupg,
  bubblewrap,
  cacert,
  callPackage,
  skillRuntime ? callPackage ./skill-runtime.nix { },
  googleClientId ? "",
  googleClientSecret ? "",
  microsoftClientId ? "",
}:
let
  root = ../..;
  manifest = builtins.fromTOML (builtins.readFile (root + /Cargo.toml));
in
rustPlatform.buildRustPackage {
  pname = "penguin-mail";
  inherit (manifest.workspace.package) version;

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions (
      [
        (root + /Cargo.toml)
        (root + /Cargo.lock)
        (root + /po)
        (root + /vendor)
        (root + /scripts/stage.sh)
        (root + /packaging/apt/penguin-mail-archive-keyring.gpg)
      ]
      ++ map (member: root + "/${member}") manifest.workspace.members
    );
  };
  cargoLock.lockFileContents = builtins.readFile (root + /Cargo.lock);

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook4
    glib
    gettext
    appstream
  ];
  buildInputs = [
    gtk4
    libadwaita
    webkitgtk_6_0
    gettext
    glib-networking
    gsettings-desktop-schemas
    adwaita-icon-theme
    shared-mime-info
  ];

  PENGUIN_MAIL_GOOGLE_CLIENT_ID = googleClientId;
  PENGUIN_MAIL_GOOGLE_CLIENT_SECRET = googleClientSecret;
  PENGUIN_MAIL_MICROSOFT_CLIENT_ID = microsoftClientId;
  PENGUIN_MAIL_SKILL_RUNTIME = skillRuntime;
  # The same setting as .cargo/config.toml, which the fileset above leaves
  # out: SQLite without its memory statistics and their global mutex.
  LIBSQLITE3_FLAGS = "-DSQLITE_DEFAULT_MEMSTATUS=0";

  passthru = { inherit skillRuntime; };

  cargoBuildFlags = [
    "-p"
    "mailrs"
    "-p"
    "mailrs-cli"
  ];
  buildFeatures = [ "mailrs/packaging-nix" ];
  # The workspace suite needs a display, a session bus, and external test servers.
  # Run it outside the package builder as documented in CONTRIBUTING.md.
  doCheck = false;

  installPhase = ''
    runHook preInstall
    CARGO_TARGET_DIR="target/${stdenv.hostPlatform.rust.rustcTarget}" \
      bash scripts/stage.sh "$out"
    runHook postInstall
  '';

  preFixup = ''
    gappsWrapperArgs+=(--prefix PATH : ${
      lib.makeBinPath [
        gnupg
        bubblewrap
      ]
    })
    gappsWrapperArgs+=(--set-default SSL_CERT_FILE ${cacert}/etc/ssl/certs/ca-bundle.crt)
  '';

  meta = {
    description = "Mail and calendar for Linux";
    homepage = "https://github.com/c9dev/penguin-mail";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "penguin-mail";
  };
}
