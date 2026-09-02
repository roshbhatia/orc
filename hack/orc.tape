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
Wait+Screen /Explorer/
Sleep 1s
Tab
Wait+Screen /Workflow/
Sleep 2s
Type "q"
