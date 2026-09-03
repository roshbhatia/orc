{
  git,
  installShellFiles,
  lib,
  makeWrapper,
  procps,
  rustPlatform,
  stdenv,
  unixtools,
}:
rustPlatform.buildRustPackage {
  pname = "orc";
  version = "0.10.0";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.lock
      ./Cargo.toml
      ./src
      ./templates
      ./tests
    ];
  };

  cargoLock = {
    lockFile = ./Cargo.lock;
    outputHashes = {
      "rs-utils-0.1.0" = "sha256-Pqei1qMrnAmjWcxX75UpqeUqRTERBb+RkxW0cWFi/8Q=";
    };
  };

  nativeBuildInputs = [
    installShellFiles
    makeWrapper
  ];

  nativeCheckInputs = [
    git
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
    "$out/bin/orc" schema provider > "$out/share/orc/provider.schema.json"
    "$out/bin/orc" schema workflow > "$out/share/orc/workflow.schema.json"
    "$out/bin/orc" schema state > "$out/share/orc/state.schema.json"
  '';

  postFixup = ''
    wrapProgram "$out/bin/orc" --prefix PATH : ${lib.makeBinPath [ git ]}
  '';

  meta.mainProgram = "orc";
}
