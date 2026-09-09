import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
ROOT = pathlib.Path(__file__).resolve().parents[1]
NAME = sys.argv[1]
META = json.loads((ROOT / 'extras' / NAME / 'demo.json').read_text())
TOOL = META['core']
OLD = 'package auth\nimport "strings"\nfunc ValidToken(header string) bool { return strings.Contains(header, "Bearer ") }\n'
NEW = 'package auth\nimport "strings"\nfunc ValidToken(header string) bool {\n if !strings.HasPrefix(header, "Bearer ") { return false }\n token := strings.TrimPrefix(header, "Bearer ")\n return token != "" && !strings.ContainsAny(token, " \\t\\n")\n}\n'
TEST = 'package auth\nimport "testing"\nfunc TestTokenBoundary(t *testing.T) {\n for _, tc := range []struct{header string; valid bool}{\n {"Bearer signed-token",true},{"",false},{"Bearer ",false},{"prefix Bearer token",false},{"Bearer two tokens",false},\n } { if got:=ValidToken(tc.header); got!=tc.valid {t.Errorf("%q: got %v want %v",tc.header,got,tc.valid)} }\n}\n'

def execute(argv, cwd, env, stdin=None, show=True, check=True):
    if show:
        display = [pathlib.Path(str(argv[0])).name, *[str(value).replace(str(cwd), '.') for value in argv[1:]]]
        print('$ ' + ' '.join(display), flush=True)
    result = subprocess.run([str(value) for value in argv], cwd=cwd, env=env, input=stdin, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=90)
    if show or result.returncode:
        print(result.stdout.rstrip().replace(str(cwd), '.'), flush=True)
    if check and result.returncode:
        raise RuntimeError('command failed: ' + str(result.returncode))
    return result

def build(package, binary, bins):
    module = ROOT
    source = ROOT / package.removeprefix('./')
    if (source / 'go.mod').exists():
        module, package = (source, '.')
    target = bins / binary
    execute(['go', 'build', '-o', target, package], module, os.environ.copy(), show=False)
    return str(target)

def shim(bins, name, body):
    target = bins / name
    target.write_text('#!/usr/bin/env python3\n' + body)
    target.chmod(493)

def demo(work, env, bins):
    script = ROOT / 'extras' / NAME / 'provider.sh'
    env['ORC_PROVIDER_LIB'] = str(ROOT / 'extras/lib/provider.sh')
    plan = {'command': ['go', 'test', '-v', './internal/auth'], 'cwd': str(work), 'environment': {}}
    request = {'version': 'orc.provider/v1', 'scope': str(work), 'plan': plan, 'session': {'harness': 'review', 'nativeId': 'token-review', 'name': 'token-review', 'id': 'token-review'}}
    if NAME == 'local':
        request['capability'] = 'execution.run'
    elif NAME == 'changes':
        request['capability'] = 'changes.inspect'
    elif NAME == 'harness':
        shim(bins, 'review', "import subprocess\nsubprocess.run(['go','test','-v','./internal/auth'],check=True)\n")
        registry = work / 'agents.json'
        registry.write_text(json.dumps({'agents': [{'name': 'review', 'command': 'review', 'launch': {}}]}))
        env['ORC_AGENT_REGISTRY'] = str(registry)
        request['capability'] = 'session.launch'
        request['command'] = ['review']
    elif NAME == 'zmx':
        request['capability'] = 'session.persist'
    elif NAME == 'wezterm':
        request.update(capability='terminal.open', direction='right')
    else:
        request['capability'] = 'messages.read'
        request['session']['harness'] = 'codex'
        print('Offline transcript fixture replay.', flush=True)
        message = {'version': 'traces.message/v1', 'id': 'review-result', 'session': 'token-review', 'timestamp': '2026-09-09T10:00:00Z', 'body': 'Reject misplaced prefixes before session lookup.'}
        shim(bins, 'traces', 'import json\nprint(json.dumps(' + repr(message) + '))\n')
    result = execute(['bash', script], work, env, json.dumps(request), show=False)
    response = json.loads(result.stdout)
    if 'command' in response:
        print('Plan: ' + ' '.join([pathlib.Path(response['command'][0]).name, *[str(value).replace(str(work), '.').replace(str(bins), '<bin>') for value in response['command'][1:]]]), flush=True)
        print('Directory: ' + response.get('cwd', str(work)).replace(str(work), '.'), flush=True)
    else:
        print(json.dumps(response), flush=True)
    if NAME in {'local', 'harness', 'traces', 'changes'}:
        selected = response if 'command' in response else response.get('plan') or response.get('output', {}).get('plan')
        if not selected:
            raise RuntimeError('provider did not produce a launch plan')
        execute(selected['command'], pathlib.Path(selected.get('cwd') or work), env)
    elif NAME in {'wezterm', 'zmx'}:
        print('Launch preview only. No active terminal session was changed.', flush=True)

def main():
    with tempfile.TemporaryDirectory(prefix='token-review-') as temporary:
        root = pathlib.Path(temporary).resolve()
        work = root / 'checkout-service'
        bins = root / 'bin'
        bins.mkdir()
        (work / 'internal/auth').mkdir(parents=True)
        (work / 'go.mod').write_text('module checkout-service\n\ngo 1.26\n')
        path = work / 'internal/auth/token.go'
        path.write_text(OLD)
        (work / 'internal/auth/token_test.go').write_text(TEST)
        env = os.environ.copy()
        for name, dirname in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'), ('XDG_DATA_HOME', 'data'), ('XDG_STATE_HOME', 'state'), ('XDG_CACHE_HOME', 'cache'), ('XDG_RUNTIME_DIR', 'runtime')]:
            (root / dirname).mkdir()
            env[name] = str(root / dirname)
        env['PATH'] = str(bins) + os.pathsep + env['PATH']
        env['XDG_DATA_DIRS'] = str(root / 'data')
        for name in ['ORC_SESSION_ID', 'ORC_SCOPE', 'WEZTERM_PANE', 'WEZTERM_UNIX_SOCKET', 'GATE_STATE_DIR']:
            env.pop(name, None)
        execute(['git', 'init', '-b', 'main'], work, env, show=False)
        execute(['git', 'config', 'user.name', 'Review fixture'], work, env, show=False)
        execute(['git', 'config', 'user.email', 'review@example.invalid'], work, env, show=False)
        execute(['git', 'add', '.'], work, env, show=False)
        execute(['git', 'commit', '-m', 'add token parser'], work, env, show=False)
        path.write_text(NEW)
        print(META['summary'] + '\n', flush=True)
        demo(work, env, bins)
        print('\nDemo complete', flush=True)
if __name__ == '__main__':
    main()
