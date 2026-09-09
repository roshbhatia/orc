Set Shell "zsh"
Set Theme "Catppuccin Macchiato"
Set FontSize 16
Set Width 1400
Set Height 760
Set Padding 18
Set Framerate 20
Set TypingSpeed 25ms

Type "cd $ORC_SCREENSHOT_SCOPE"
Enter
Sleep 500ms
Type "orc status"
Enter
Sleep 1s
Type "orc run list"
Enter
Sleep 1s
Type "go test -v ./internal/auth"
Enter
Sleep 2s
