Set Shell "zsh"
Set Theme "Catppuccin Macchiato"
Set FontSize 16
Set Width 1800
Set Height 900
Set Padding 18
Set Framerate 20
Set TypingSpeed 0

Sleep 1s

Type "orc tui --scope $ORC_SCREENSHOT_SCOPE"
Enter
Wait+Screen /renderer-provider-migration/
Sleep 1s
Type "j"
Sleep 250ms
Type "g"
Wait+Screen /workflow/
Sleep 3500ms
Screenshot docs/orc.png
Type "l"
Sleep 250ms
Ctrl+L
Sleep 250ms
Ctrl+H
Sleep 250ms
Type "p"
Sleep 500ms
Wait+Screen /integrations/
Sleep 500ms
Type "v"
Sleep 500ms
Escape
Type "q"
