Output extras/github-aw/demo.gif
Require orc
Require gh
Set Shell "bash"
Set Width 1400
Set Height 900
Set FontSize 18
Set TypingSpeed 40ms
Set WaitTimeout 60s
Hide
Type "export PS1='$ '; clear"
Enter
Show
Sleep 1s
Type 'orc get executions factory-docs-credentials --scope "$ORC_FACTORY_SCOPE"'
Enter
Wait+Screen /Succeeded/
Sleep 6s
Type 'gh run view "${ORC_FACTORY_RUN_URL##*/}" --repo roshbhatia/sysinit.laurel'
Enter
Wait+Screen /conclusion/
Sleep 8s
Type 'gh pr view "$ORC_FACTORY_PR_URL" --json title,isDraft,url,files'
Enter
Wait+Screen /isDraft/
Sleep 10s
