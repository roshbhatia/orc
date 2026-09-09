Require bash
Set Shell "bash"
Set Theme "Catppuccin Macchiato"
Set FontSize 17
Set Width 1200
Set Height 680
Set Framerate 10
Set TypingSpeed 15ms
Set WaitTimeout 120s
Output "extras/wezterm/demo.gif"
Hide
Type "export PS1='$ '; clear"
Enter
Sleep 300ms
Show
Type "bash extras/wezterm/demo.sh"
Enter
Wait+Screen /Demo complete/
Sleep 2s
