Output extras/github-aw/demo.gif
Set Shell "bash"
Set Width 1200
Set Height 760
Set FontSize 18
Set TypingSpeed 65ms
Type "orc get executions"
Enter
Sleep 6s
Type "gh run view \"${ORC_FACTORY_RUN_URL##*/}\" --repo roshbhatia/sysinit.laurel"
Enter
Sleep 10s
