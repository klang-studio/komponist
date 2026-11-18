//! Saved routes use full JACK port names. Aliases are presentation only.

use serde::{Deserialize, Serialize};

/// Linux JACK routing for the application's capture channels and stereo master.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct JackRoutes {
    /// In capture channel order. None uses the legacy input-device setting.
    /// An explicitly empty list disables capture.
    pub capture: Option<Vec<String>>,
    /// Left and right. Two empty slots use JACK's legacy automatic playback routes.
    pub playback: [Option<String>; 2],
}

impl JackRoutes {
    /// Whether there is an explicit port selection or capture override.
    pub fn is_automatic(&self) -> bool {
        self.capture.is_none() && self.playback.iter().all(Option::is_none)
    }
}

/// JACK direction from komponist's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JackDirection {
    /// A JACK output port from which komponist can receive audio.
    Capture,
    /// A JACK input port to which komponist can send audio.
    Playback,
}

/// An audio port available in the current JACK graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JackPort {
    /// Full JACK port name, unique in the graph.
    pub name: String,
    /// Optional readable name. The full name distinguishes ports sharing an alias.
    pub alias: Option<String>,
    /// Whether the port can be used for capture or playback.
    pub direction: JackDirection,
}

impl JackPort {
    /// Display both the alias and full name when they differ.
    pub fn label(&self) -> String {
        match self.alias.as_deref() {
            Some(alias) if alias != self.name => format!("{alias} · {}", self.name),
            _ => self.name.clone(),
        }
    }
}

/// Enumerates audio ports without activating a stream or starting a JACK server.
pub fn jack_ports() -> Result<Vec<JackPort>, crate::AudioError> {
    #[cfg(target_os = "linux")]
    {
        crate::jack_backend::ports()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(crate::AudioError::HostUnavailable(crate::HostKind::Jack))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jack_routes_round_trip_through_stream_settings() {
        let legacy = r#"{"host":"jack","device":null,"sample_rate":48000,"buffer_frames":64}"#;
        let mut settings: crate::StreamConfig = serde_json::from_str(legacy).unwrap();
        assert!(settings.jack_routes.is_automatic());
        assert!(
            !serde_json::to_string(&settings)
                .unwrap()
                .contains("jack_routes")
        );
        settings.jack_routes = JackRoutes {
            capture: Some(vec!["testing:right".into(), "testing:left".into()]),
            playback: [None, Some("testing:monitor_right".into())],
        };
        let serialized = serde_json::to_string(&settings).unwrap();
        let loaded: crate::StreamConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(loaded.jack_routes, settings.jack_routes);
        settings.jack_routes.capture = Some(Vec::new());
        let serialized = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<crate::StreamConfig>(&serialized)
                .unwrap()
                .jack_routes
                .capture,
            Some(Vec::new())
        );
    }
}
