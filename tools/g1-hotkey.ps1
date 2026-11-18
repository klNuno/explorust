# Presses Ctrl+Alt+Shift+F12 through SendInput, as a user would, to test the guard's escape hotkey.
# Run on the user's desktop: os win run g1 --desktop -- powershell -File C:\explorust\g1-hotkey.ps1
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Keys {
    [StructLayout(LayoutKind.Sequential)] struct KI { public ushort vk, scan; public uint flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Explicit, Size = 40)] struct IN { [FieldOffset(0)] public uint type; [FieldOffset(8)] public KI ki; }
    [DllImport("user32.dll")] static extern uint SendInput(uint n, IN[] inputs, int size);
    static IN K(ushort vk, bool up) { var i = new IN(); i.type = 1; i.ki.vk = vk; i.ki.flags = up ? 2u : 0u; return i; }
    public static uint Combo() {
        ushort[] mods = { 0x11, 0x12, 0x10 }; // Ctrl, Alt, Shift
        var seq = new System.Collections.Generic.List<IN>();
        foreach (var m in mods) seq.Add(K(m, false));
        seq.Add(K(0x7B, false)); seq.Add(K(0x7B, true)); // F12
        for (int j = mods.Length - 1; j >= 0; j--) seq.Add(K(mods[j], true));
        return SendInput((uint)seq.Count, seq.ToArray(), Marshal.SizeOf(typeof(IN)));
    }
}
'@
"sent $([Keys]::Combo()) inputs" | Out-File C:\explorust\hotkey.txt
