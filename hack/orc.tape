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
Enter
Wait+Screen /workflow/
Sleep 500ms
Screenshot docs/orc.png
Type "l"
Sleep 250ms
Ctrl+L
Sleep 250ms
Type "]"
Sleep 500ms
Ctrl+H
Sleep 250ms
Tab
Sleep 500ms
Type "0"
Wait+Screen /integrations/
Sleep 500ms
Type "v"
Sleep 500ms
Escape
Type "q"
