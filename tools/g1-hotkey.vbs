' Presses Ctrl+Alt+Shift+F12 with Notepad in front (a medium-integrity window, so the guard's
' low-level hook sees the keys). Run: os win run g1 --desktop -- wscript C:\explorust\g1-hotkey.vbs
Set sh = CreateObject("WScript.Shell")
sh.Run "notepad.exe", 1, False
WScript.Sleep 4000
ok = sh.AppActivate("Notepad")
sh.SendKeys "^%+{F12}"
Set f = CreateObject("Scripting.FileSystemObject").CreateTextFile("C:\explorust\hotkey.txt", True)
f.WriteLine "appactivate=" & ok
f.Close
