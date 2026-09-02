Set Shell "zsh"
Set Theme "Catppuccin Macchiato"
Set FontSize 16
Set Width 1800
Set Height 900
Set Padding 18
Set Framerate 20
Set TypingSpeed 0

Sleep 1s

Type "$ORC_SCREENSHOT_BIN tui --scope $ORC_SCREENSHOT_SCOPE"
Enter
Wait+Screen /Renderer provider migration/
Sleep 1s
Type "j"
Sleep 250ms
Type "t"
Sleep 500ms
Type "f"
Sleep 500ms
Type "g"
Sleep 500ms
Ctrl+L
Sleep 250ms
Type "j"
Sleep 250ms
Ctrl+H
Sleep 250ms
Type "2"
Wait+Screen /providers/
Sleep 500ms
Type "1"
Sleep 500ms
Type "g"
Wait+Screen /Renderer provider migration/
Sleep 1s
Type "q"
