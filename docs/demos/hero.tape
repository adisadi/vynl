# VHS demo recording script for vynl (https://github.com/charmbracelet/vhs)
Output docs/demos/hero.gif

Set FontSize 14
Set Width 1280
Set Height 720
Set Theme "Catppuccin Mocha"

Type "vynl doctor"
Enter
Sleep 2s

Type "vynl --demo"
Enter
Sleep 2s

# Open Command Palette (Ctrl+P) to demonstrate Modal-Aware Image Occlusion
Ctrl+P
Sleep 1.5s
Type "tokyo"
Sleep 1s
Enter
Sleep 1.5s

# Toggle Zen Fullscreen Now Playing mode
Type "z"
Sleep 2.5s

# Quit cleanly
Type "Q"
Sleep 500ms
