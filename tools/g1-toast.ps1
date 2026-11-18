# Shows a toast as PowerShell, to test toasts and the notification center without explorer.
# Run: os win run g1 --desktop -- powershell -WindowStyle Hidden -File C:\explorust\g1-toast.ps1
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument
$xml.LoadXml('<toast><visual><binding template="ToastGeneric"><text>explorust</text><text>toast without explorer</text></binding></visual></toast>')
$aumid = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
try {
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($aumid).Show([Windows.UI.Notifications.ToastNotification]::new($xml))
    'shown' | Out-File C:\explorust\toast.txt
} catch { "error $_" | Out-File C:\explorust\toast.txt }
