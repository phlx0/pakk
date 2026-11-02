Output packaging/demo.gif
Set FontSize 18
Set Width 1100
Set Height 520
Set Theme "Catppuccin Mocha"

Type "time zip -qr docs.zip rustdocs" Enter
Sleep 10s
Type "time pakk pack rustdocs" Enter
Sleep 3s
Type "pakk test rustdocs.zip" Enter
Sleep 3s
Type "rm -rf rustdocs && pakk unpack rustdocs.zip" Enter
Sleep 4s
