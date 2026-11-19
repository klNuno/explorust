//! The state behind the quick settings glyphs: the default speaker's volume and mute
//! (IAudioEndpointVolume, with its change callback), the network (INetworkListManager, with its
//! connectivity events, and the adapter type) and the battery (GetSystemPowerStatus, refreshed on
//! WM_POWERBROADCAST). Callbacks post `msg` to the bar, which reads the state again.

use super::log;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{AUDIO_VOLUME_NOTIFICATION_DATA, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender};
use windows::Win32::NetworkManagement::IpHelper::{GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::NetworkListManager::{
    INetworkListManager, INetworkListManagerEvents, INetworkListManagerEvents_Impl, NLM_CONNECTIVITY,
    NLM_CONNECTIVITY_IPV4_INTERNET, NLM_CONNECTIVITY_IPV6_INTERNET, NetworkListManager,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, IConnectionPoint, IConnectionPointContainer};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows_core::{Interface, implement};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Net {
    Wired,
    Wifi,
    /// Connected without internet (wired or not).
    Limited(bool),
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    /// (level 0..1, muted); None without an output device.
    pub volume: Option<(f32, bool)>,
    pub net: Net,
    /// (percent, charging); None without a battery.
    pub battery: Option<(u8, bool)>,
}

impl Status {
    /// The Segoe Fluent Icons glyphs, left to right: network, volume, battery.
    pub fn glyphs(&self) -> Vec<char> {
        let net = match self.net {
            Net::Wired => '\u{E839}',
            Net::Wifi => '\u{E701}',
            Net::Limited(true) => '\u{EB55}',
            Net::Limited(false) => '\u{EB63}',
            Net::Offline => '\u{F384}',
        };
        let vol = match self.volume {
            None => '\u{E74F}',
            Some((_, true)) => '\u{E74F}',
            Some((v, false)) if v <= 0.001 => '\u{E992}',
            Some((v, false)) if v < 0.33 => '\u{E993}',
            Some((v, false)) if v < 0.66 => '\u{E994}',
            Some(_) => '\u{E995}',
        };
        let mut out = vec![net, vol];
        if let Some((p, charging)) = self.battery {
            let step = (p.min(100) as u32 + 5) / 10;
            out.push(char::from_u32(if charging { 0xEBAB } else { 0xEBA0 } + step).unwrap_or('\u{EBA0}'));
        }
        out
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeSink {
    hwnd: isize,
    msg: u32,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeSink_Impl {
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows_core::Result<()> {
        // SAFETY: posts to the bar from the audio thread.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), self.msg, WPARAM(0), LPARAM(0)) };
        Ok(())
    }
}

#[implement(INetworkListManagerEvents)]
struct NetSink {
    hwnd: isize,
    msg: u32,
}

impl INetworkListManagerEvents_Impl for NetSink_Impl {
    fn ConnectivityChanged(&self, _: NLM_CONNECTIVITY) -> windows_core::Result<()> {
        // SAFETY: posts to the bar.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), self.msg, WPARAM(0), LPARAM(0)) };
        Ok(())
    }
}

pub struct Watch {
    volume: Option<(IAudioEndpointVolume, IAudioEndpointVolumeCallback)>,
    nlm: Option<INetworkListManager>,
    advise: Option<(IConnectionPoint, u32)>,
}

impl Watch {
    /// Opens the sources and registers the callbacks; COM must be up on this thread.
    pub fn new(hwnd: HWND, msg: u32) -> Watch {
        let h = hwnd.0 as isize;
        // SAFETY: COM calls on this STA thread; the interfaces are kept and released in Drop.
        unsafe {
            let volume = (|| -> windows_core::Result<_> {
                let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let dev = e.GetDefaultAudioEndpoint(eRender, eConsole)?;
                let v: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None)?;
                let cb: IAudioEndpointVolumeCallback = VolumeSink { hwnd: h, msg }.into();
                v.RegisterControlChangeNotify(&cb)?;
                Ok((v, cb))
            })();
            if let Err(e) = &volume {
                log::line(&format!("volume: no endpoint ({e})"));
            }
            let nlm: Option<INetworkListManager> = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL).ok();
            let advise = nlm.as_ref().and_then(|n| {
                let c: IConnectionPointContainer = n.cast().ok()?;
                let cp = c.FindConnectionPoint(&INetworkListManagerEvents::IID).ok()?;
                let sink: INetworkListManagerEvents = NetSink { hwnd: h, msg }.into();
                let cookie = cp.Advise(&sink).ok()?;
                Some((cp, cookie))
            });
            log::line(&format!("status watch: volume {}, network events {}", volume.is_ok(), advise.is_some()));
            Watch { volume: volume.ok(), nlm, advise }
        }
    }

    pub fn read(&self) -> Status {
        // SAFETY: COM reads on this thread and a plain power query.
        unsafe {
            let volume = self.volume.as_ref().and_then(|(v, _)| {
                let level = v.GetMasterVolumeLevelScalar().ok()?;
                let mute = v.GetMute().ok()?.as_bool();
                Some((level, mute))
            });
            let conn = self.nlm.as_ref().and_then(|n| n.GetConnectivity().ok()).unwrap_or_default();
            let internet = conn.0 & (NLM_CONNECTIVITY_IPV4_INTERNET.0 | NLM_CONNECTIVITY_IPV6_INTERNET.0) != 0;
            let (wired, wifi) = adapters_up();
            let net = match (internet, wifi && !wired, wired || wifi) {
                (true, true, _) => Net::Wifi,
                (true, false, _) => Net::Wired,
                (false, w, true) => Net::Limited(!w),
                (false, _, false) => Net::Offline,
            };
            let mut ps = SYSTEM_POWER_STATUS::default();
            let battery = GetSystemPowerStatus(&mut ps)
                .ok()
                .filter(|_| ps.BatteryFlag & 128 == 0 && ps.BatteryFlag != 255 && ps.BatteryLifePercent <= 100)
                .map(|_| (ps.BatteryLifePercent, ps.ACLineStatus == 1));
            Status { volume, net, battery }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: unregisters what `new` registered.
        unsafe {
            if let Some((v, cb)) = &self.volume {
                let _ = v.UnregisterControlChangeNotify(cb);
            }
            if let Some((cp, cookie)) = &self.advise {
                let _ = cp.Unadvise(*cookie);
            }
        }
    }
}

/// (an Ethernet adapter is up, a Wi-Fi adapter is up), loopback and tunnels left out.
fn adapters_up() -> (bool, bool) {
    let mut len = 16 * 1024u32;
    let mut buf = vec![0u64; len as usize / 8];
    // SAFETY: GetAdaptersAddresses fills a buffer we own; the list is walked inside it.
    unsafe {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST;
        let mut r = GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr().cast()), &mut len);
        if r == 111 {
            buf = vec![0u64; len as usize / 8 + 1];
            r = GetAdaptersAddresses(0, flags, None, Some(buf.as_mut_ptr().cast()), &mut len);
        }
        if r != 0 {
            return (false, false);
        }
        let (mut wired, mut wifi) = (false, false);
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = &*p;
            if a.OperStatus == IfOperStatusUp {
                match a.IfType {
                    6 => wired = true,
                    71 => wifi = true,
                    _ => {}
                }
            }
            p = a.Next;
        }
        (wired, wifi)
    }
}
