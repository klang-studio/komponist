//! Audio settings discovery and its worker results.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use komponist_audio::{AudioError, AudioSystem, DeviceInfo, HostKind, InputDeviceInfo, JackPort};

use komponist_session::engine_sync::EngineProcess;

/// How often to refresh the JACK graph while settings are open.
pub(crate) const JACK_REFRESH: Duration = Duration::from_secs(1);

/// Where the dialog gets drivers and devices from: the system ([`SystemDevices`]) or a fixed
/// list (tests, `App::headless`).
pub trait AudioDevices: Send + Sync {
    /// Available drivers (`AudioSystem::available_hosts`).
    fn hosts(&self) -> Vec<HostKind>;
    /// Output devices of `host`; empty if it cannot be enumerated.
    fn devices(&self, host: HostKind) -> Vec<DeviceInfo>;
    /// [`AudioDevices::devices`], telling why a list is empty when the driver failed. The dialog
    /// shows the reason; providers that cannot fail keep the default.
    fn try_devices(&self, host: HostKind) -> Result<Vec<DeviceInfo>, AudioError> {
        Ok(self.devices(host))
    }
    /// Capture devices; providers used by headless tests expose none unless explicitly set.
    fn input_devices(&self, _host: HostKind) -> Vec<komponist_audio::InputDeviceInfo> {
        Vec::new()
    }
    /// [`AudioDevices::input_devices`] with the driver's error.
    fn try_input_devices(
        &self,
        host: HostKind,
    ) -> Result<Vec<komponist_audio::InputDeviceInfo>, AudioError> {
        Ok(self.input_devices(host))
    }
    /// JACK graph metadata; the fixed/headless provider never contacts a real JACK server.
    fn jack_ports(&self) -> Result<Vec<JackPort>, AudioError> {
        Ok(Vec::new())
    }
    /// Whether enumeration may take long enough to stall a frame, so the dialog runs it on a
    /// worker thread and shows the previous list meanwhile (WP-61 measured 7–10 ms for the
    /// drivers, whose JACK probe opens a client, and 170–200 ms for ALSA's devices).
    fn is_slow(&self) -> bool {
        false
    }
}

/// The system's drivers and devices (cpal).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDevices;

impl AudioDevices for SystemDevices {
    fn hosts(&self) -> Vec<HostKind> {
        AudioSystem::<EngineProcess>::available_hosts()
    }

    fn devices(&self, host: HostKind) -> Vec<DeviceInfo> {
        self.try_devices(host).unwrap_or_default()
    }

    fn try_devices(&self, host: HostKind) -> Result<Vec<DeviceInfo>, AudioError> {
        AudioSystem::<EngineProcess>::devices(host)
    }

    fn input_devices(&self, host: HostKind) -> Vec<komponist_audio::InputDeviceInfo> {
        self.try_input_devices(host).unwrap_or_default()
    }

    fn try_input_devices(
        &self,
        host: HostKind,
    ) -> Result<Vec<komponist_audio::InputDeviceInfo>, AudioError> {
        AudioSystem::<EngineProcess>::input_devices(host)
    }

    fn jack_ports(&self) -> Result<Vec<JackPort>, AudioError> {
        komponist_audio::jack_ports()
    }

    fn is_slow(&self) -> bool {
        true
    }
}

/// Runs `f` with `devices` on a worker thread if enumeration is slow, else at once; the result
/// comes through the receiver.
fn enumerate<T: Send + 'static>(
    devices: &Arc<dyn AudioDevices>,
    f: impl FnOnce(&dyn AudioDevices) -> T + Send + 'static,
) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    if devices.is_slow() {
        let devices = Arc::clone(devices);
        let spawned = std::thread::Builder::new()
            .name("komponist-audio-devices".into())
            .spawn(move || {
                let _ = tx.send(f(&*devices));
            });
        if spawned.is_err() {
            // No thread: the dialog keeps the list it has.
            return rx;
        }
    } else {
        let _ = tx.send(f(&**devices));
    }
    rx
}

/// A fixed list of drivers and devices.
#[derive(Debug, Default, Clone)]
pub struct FixedDevices {
    /// Available drivers.
    pub hosts: Vec<HostKind>,
    /// Devices of all drivers.
    pub devices: Vec<DeviceInfo>,
}

impl AudioDevices for FixedDevices {
    fn hosts(&self) -> Vec<HostKind> {
        self.hosts.clone()
    }

    fn devices(&self, host: HostKind) -> Vec<DeviceInfo> {
        self.devices
            .iter()
            .filter(|d| d.host == host)
            .cloned()
            .collect()
    }
}

type Pending<T> = Receiver<Result<Vec<T>, AudioError>>;

#[derive(Default)]
pub(crate) struct AudioInventory {
    pub(crate) hosts: Vec<HostKind>,
    pub(crate) devices: Vec<DeviceInfo>,
    pub(crate) devices_error: Option<String>,
    pub(crate) inputs: Vec<InputDeviceInfo>,
    pub(crate) inputs_error: Option<String>,
    pub(crate) jack_ports: Option<Vec<JackPort>>,
    pub(crate) jack_scan_failed: bool,
}

pub(crate) struct AudioSettings {
    provider: Arc<dyn AudioDevices>,
    inventory: AudioInventory,
    devices_host: Option<HostKind>,
    input_host: Option<HostKind>,
    pending_hosts: Option<Receiver<Vec<HostKind>>>,
    pending_devices: Option<(HostKind, Pending<DeviceInfo>)>,
    pending_inputs: Option<(HostKind, Pending<InputDeviceInfo>)>,
    pending_jack_ports: Option<Pending<JackPort>>,
    next_jack_refresh: Option<Instant>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            provider: Arc::new(SystemDevices),
            inventory: AudioInventory::default(),
            devices_host: None,
            input_host: None,
            pending_hosts: None,
            pending_devices: None,
            pending_inputs: None,
            pending_jack_ports: None,
            next_jack_refresh: None,
        }
    }
}

impl AudioSettings {
    pub(crate) fn set_provider(&mut self, provider: Arc<dyn AudioDevices>) {
        self.provider = provider;
    }

    pub(crate) fn provider(&self) -> Arc<dyn AudioDevices> {
        Arc::clone(&self.provider)
    }

    /// Lists the output and capture devices and refreshes the JACK graph on the next scan.
    pub(crate) fn refresh(&mut self) {
        self.devices_host = None;
        self.input_host = None;
        self.next_jack_refresh = None;
    }

    pub(crate) fn enumerating(&self) -> bool {
        self.pending_hosts.is_some()
            || self.pending_devices.is_some()
            || self.pending_inputs.is_some()
            || self.pending_jack_ports.is_some()
    }

    pub(crate) fn scan(&mut self, host: HostKind, opening: bool, now: Instant) -> &AudioInventory {
        if opening {
            self.pending_hosts = Some(enumerate(&self.provider, |devices| devices.hosts()));
            self.refresh();
        }
        let requested = self.pending_devices.as_ref().map(|(host, _)| *host);
        if self.devices_host != Some(host) && requested != Some(host) {
            let rx = enumerate(&self.provider, move |d| d.try_devices(host));
            self.pending_devices = Some((host, rx));
        }
        if self.input_host != Some(host)
            && self.pending_inputs.as_ref().map(|(host, _)| *host) != Some(host)
        {
            self.pending_inputs = Some((
                host,
                enumerate(&self.provider, move |provider| {
                    provider.try_input_devices(host)
                }),
            ));
        }
        if host == HostKind::Jack
            && self.pending_jack_ports.is_none()
            && self.next_jack_refresh.is_none_or(|next| now >= next)
        {
            self.pending_jack_ports = Some(enumerate(&self.provider, |d| d.jack_ports()));
            self.next_jack_refresh = Some(now + JACK_REFRESH);
        }
        self.collect();
        &self.inventory
    }

    /// Takes finished enumerations.
    fn collect(&mut self) {
        if let Some(rx) = &self.pending_jack_ports {
            match rx.try_recv() {
                Ok(Ok(ports)) => {
                    self.inventory.jack_ports = Some(ports);
                    self.inventory.jack_scan_failed = false;
                    self.pending_jack_ports = None;
                }
                Err(TryRecvError::Empty) => {}
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                    self.inventory.jack_scan_failed = true;
                    self.pending_jack_ports = None;
                }
            }
        }
        if let Some((host, rx)) = &self.pending_inputs {
            match rx.try_recv() {
                Ok(inputs) => {
                    self.inventory.inputs_error = inputs.as_ref().err().map(ToString::to_string);
                    self.inventory.inputs = inputs.unwrap_or_default();
                    self.input_host = Some(*host);
                    self.pending_inputs = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.pending_inputs = None,
            }
        }
        if let Some(rx) = &self.pending_hosts {
            match rx.try_recv() {
                Ok(hosts) => {
                    self.inventory.hosts = hosts;
                    self.pending_hosts = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.pending_hosts = None,
            }
        }
        if let Some((host, rx)) = &self.pending_devices {
            match rx.try_recv() {
                Ok(devices) => {
                    self.inventory.devices_error = devices.as_ref().err().map(ToString::to_string);
                    self.inventory.devices = devices.unwrap_or_default();
                    self.devices_host = Some(*host);
                    self.pending_devices = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.pending_devices = None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_host_replaces_the_output_inventory() {
        let mut settings = AudioSettings::default();
        settings.set_provider(Arc::new(FixedDevices {
            hosts: vec![HostKind::Null, HostKind::Jack],
            devices: Vec::new(),
        }));
        let now = Instant::now();
        assert_eq!(
            settings.scan(HostKind::Null, true, now).hosts,
            [HostKind::Null, HostKind::Jack]
        );
        assert!(!settings.enumerating());
        assert_eq!(settings.devices_host, Some(HostKind::Null));

        assert_eq!(
            settings.scan(HostKind::Jack, false, now).jack_ports,
            Some(Vec::new())
        );
        assert!(!settings.enumerating());
        assert_eq!(settings.devices_host, Some(HostKind::Jack));
        assert_eq!(settings.input_host, Some(HostKind::Jack));
        settings.refresh();
        assert!(settings.devices_host.is_none() && settings.input_host.is_none());
    }

    use komponist_audio::JackDirection;

    #[test]
    fn failed_jack_refresh_keeps_the_last_inventory() {
        let port = JackPort {
            name: "Interface:left".into(),
            alias: None,
            direction: JackDirection::Capture,
        };
        let mut cache = AudioSettings::default();
        let mut complete = |result| {
            let (sender, receiver) = mpsc::channel();
            sender.send(result).unwrap();
            cache.pending_jack_ports = Some(receiver);
            cache.collect();
            (
                cache.inventory.jack_ports.clone(),
                cache.inventory.jack_scan_failed,
            )
        };

        assert_eq!(
            complete(Ok(vec![port.clone()])),
            (Some(vec![port.clone()]), false)
        );
        assert_eq!(
            complete(Err(AudioError::Backend("scan unavailable".into()))),
            (Some(vec![port]), true),
        );
        assert_eq!(complete(Ok(Vec::new())), (Some(Vec::new()), false));
    }
}
