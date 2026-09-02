Set Shell "zsh"
Set Theme "Catppuccin Macchiato"
Set FontSize 16
Set Width 1400
Set Height 760
Set Padding 18
Set Framerate 20
Set TypingSpeed 25ms

Type "$ORC_SCREENSHOT_BIN status --scope $ORC_SCREENSHOT_SCOPE"
Enter
Sleep 1s
Type "$ORC_SCREENSHOT_BIN run list --scope $ORC_SCREENSHOT_SCOPE"
Enter
Sleep 1s
Type "$ORC_SCREENSHOT_BIN run show $ORC_SCREENSHOT_RUN --scope $ORC_SCREENSHOT_SCOPE"
Enter
Sleep 2s
