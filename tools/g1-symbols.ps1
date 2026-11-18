# Damages or restores the guest's twinui symbol manifests to test the guard's fallback.
#   corrupt: back up every nickel-uwu-symbols.txt, then shift each RVA by 0x40 (calls land mid-function)
#   restore: put the backups back
param([ValidateSet('corrupt', 'restore')][string]$Mode, [string]$User = 'agent')
$root = "C:\Users\$User\AppData\Local\Nickel\symbols"
foreach ($f in Get-ChildItem -Recurse -Filter nickel-uwu-symbols.txt $root) {
    $bak = "$($f.FullName).bak"
    if ($Mode -eq 'corrupt') {
        Copy-Item $f.FullName $bak -Force
        $rows = Get-Content $f.FullName | ForEach-Object {
            $name, $rva = $_ -split "`t"
            "$name`t{0:x}" -f ([Convert]::ToInt64($rva, 16) + 0x40)
        }
        [IO.File]::WriteAllText($f.FullName, (($rows -join "`n") + "`n"))
        "corrupted $($f.FullName)"
    } elseif (Test-Path $bak) {
        Move-Item $bak $f.FullName -Force
        "restored $($f.FullName)"
    }
}
