{
  python3,
  runCommand,
  uv,
  writeShellApplication,
}:
let
  wheelBuilder = python3.withPackages (packages: [ packages.wheel ]);
  wheels = runCommand "orc-zmx-python-wheels" { nativeBuildInputs = [ wheelBuilder ]; } ''
    mkdir -p package "$out"
    cp -R ${python3.pkgs.psutil}/${python3.sitePackages}/. package/
    chmod -R u+w package
    python -m wheel pack package --dest-dir "$out"
  '';
in
writeShellApplication {
  name = "orc-zmx-python";
  text = ''
    exec ${uv}/bin/uv run --offline --no-managed-python --no-python-downloads --no-project \
      --python ${python3}/bin/python3 --no-index --find-links ${wheels} \
      --script "$@"
  '';
}
