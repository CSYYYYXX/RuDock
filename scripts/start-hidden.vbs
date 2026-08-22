Option Explicit
Dim shell, root, command
Set shell = CreateObject("WScript.Shell")
root = CreateObject("Scripting.FileSystemObject").GetParentFolderName(WScript.ScriptFullName)
shell.CurrentDirectory = root
command = """" & root & "\wb.exe"" daemon start"
shell.Run command, 0, False
