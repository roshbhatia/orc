{
  git,
  installShellFiles,
  jq,
  lib,
  makeWrapper,
  procps,
  rustPlatform,
  stdenv,
  unixtools,
}:
rustPlatform.buildRustPackage {
  pname = "orc";
  version = "0.10.1";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.lock
      ./Cargo.toml
      ./assets
      ./extras/lib/provider.sh
      ./extras/local/provider.sh
      ./src
      ./templates
      ./tests
    ];
  };

  cargoLock = {
    lockFile = ./Cargo.lock;
    outputHashes = {
      "rs-utils-0.1.0" = "sha256-0gIfKOaD9Q0E4JkMKNvxgN1r2SK0OMVtmBP6dlViGCE=";
    };
  };

  nativeBuildInputs = [
    installShellFiles
    makeWrapper
  ];

  postPatch = ''
    patchShebangs extras
  '';

  nativeCheckInputs = [
    git
    jq
  ]
  ++ lib.optionals stdenv.hostPlatform.isDarwin [ unixtools.ps ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [ procps ];

  postInstall = ''
    installShellCompletion --cmd orc \
      --bash <($out/bin/orc completion bash) \
      --fish <($out/bin/orc completion fish) \
      --zsh <($out/bin/orc completion zsh)
    mkdir -p "$out/share/nushell/vendor/autoload" "$out/share/orc"
    "$out/bin/orc" completion nu > "$out/share/nushell/vendor/autoload/orc.nu"
    "$out/bin/orc" schema config > "$out/share/orc/config.schema.json"
    "$out/bin/orc" schema animation > "$out/share/orc/terminal.animation.v1.schema.json"
    "$out/bin/orc" schema resource > "$out/share/orc/resource.schema.json"
    "$out/bin/orc" schema provider > "$out/share/orc/provider.schema.json"
    "$out/bin/orc" schema workflow > "$out/share/orc/workflow.schema.json"
    "$out/bin/orc" schema state > "$out/share/orc/state.schema.json"
    install -Dm644 ${./assets/animations.yaml} "$out/share/orc/animations.yaml"
  '';

  postFixup = ''
    wrapProgram "$out/bin/orc" --prefix PATH : ${lib.makeBinPath [ git ]}
  '';

  meta.mainProgram = "orc";
}
