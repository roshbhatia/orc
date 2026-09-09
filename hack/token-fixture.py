#!/usr/bin/env python3
"""Create the local checkout-service regression fixture used in recordings."""
from pathlib import Path
import subprocess
import sys
root = Path(sys.argv[1]).resolve()
(root / 'internal/auth').mkdir(parents=True, exist_ok=True)
(root / 'go.mod').write_text('module checkout-service\n\ngo 1.26\n')
(root / 'internal/auth/token.go').write_text('package auth\nimport "strings"\nfunc ValidToken(header string) bool {\n if !strings.HasPrefix(header, "Bearer ") { return false }\n token := strings.TrimPrefix(header, "Bearer ")\n return token != "" && !strings.ContainsAny(token, " \\t\\n")\n}\n')
(root / 'internal/auth/token_test.go').write_text('package auth\nimport "testing"\nfunc TestTokenBoundary(t *testing.T) {\n for _, tc := range []struct{header string; valid bool}{\n {"Bearer signed-token",true},{"",false},{"Bearer ",false},{"prefix Bearer token",false},{"Bearer two tokens",false},\n } { if got:=ValidToken(tc.header); got!=tc.valid {t.Errorf("%q: got %v want %v",tc.header,got,tc.valid)} }\n}\n')
subprocess.run(['gofmt', '-w', 'internal/auth'], cwd=root, check=True)
