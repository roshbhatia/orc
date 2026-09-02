{
  git,
  installShellFiles,
  lib,
  makeWrapper,
  rustPlatform,
}:
rustPlatform.buildRustPackage {
  pname = "orc";
  version = "0.7.0";
  src = ./.;

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [
    installShellFiles
    makeWrapper
  ];

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
