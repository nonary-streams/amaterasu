use amaterasu_media::{CaptureGeometry, DesktopRect};
#[cfg(any(windows, test))]
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedOutput {
    pub source_name: String,
    pub monitor_handle: usize,
    pub rectangle: DesktopRect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureSelection {
    pub geometry: CaptureGeometry,
    pub target_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryError {
    Unavailable(&'static str),
    TargetChanged,
}

impl fmt::Display for GeometryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) => formatter.write_str(message),
            Self::TargetChanged => formatter.write_str("captured physical display target changed"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(any(windows, test))]
pub(crate) struct DisplaySnapshot {
    pub source_name: String,
    pub monitor_handle: usize,
    pub rectangle: DesktopRect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(any(windows, test))]
pub(crate) struct CcdSnapshot {
    pub source_name: String,
    pub target_id: String,
    pub rectangle: DesktopRect,
}

#[cfg(any(windows, test))]
pub(crate) struct Topology {
    displays: Vec<VerifiedDisplay>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(any(windows, test))]
struct VerifiedDisplay {
    source_name: String,
    target_id: String,
    monitor_handle: usize,
    rectangle: DesktopRect,
}

#[cfg(any(windows, test))]
impl Topology {
    /// Select one exact target from the fully reconciled topology. The caller
    /// keeps its private target identity inside the provider's matcher.
    pub(crate) fn select_target(
        &self,
        mut matches: impl FnMut(&str) -> bool,
    ) -> Result<SelectedOutput, GeometryError> {
        let mut matches = self
            .displays
            .iter()
            .filter(|display| matches(&display.target_id));
        let display = matches.next().ok_or(GeometryError::TargetChanged)?;
        if matches.next().is_some() {
            return Err(unavailable(
                "exact capture selector matches more than one target",
            ));
        }
        Ok(SelectedOutput {
            source_name: display.source_name.clone(),
            monitor_handle: display.monitor_handle,
            rectangle: display.rectangle,
        })
    }

    pub(crate) fn reconcile(
        gdi: Vec<DisplaySnapshot>,
        dxgi: Vec<DisplaySnapshot>,
        ccd: Vec<CcdSnapshot>,
    ) -> Result<Self, GeometryError> {
        if gdi.is_empty() || gdi.len() != dxgi.len() || gdi.len() != ccd.len() {
            return Err(unavailable(
                "Windows display enumerators report different active sets",
            ));
        }
        let mut dxgi = unique_handles(dxgi, "DXGI reports duplicate monitor handles")?;
        let mut ccd = unique_sources(ccd)?;
        let mut gdi_handles = BTreeMap::new();
        let mut gdi_sources = BTreeMap::new();
        let mut target_ids = BTreeMap::new();
        let mut displays = Vec::with_capacity(gdi.len());
        for gdi_display in gdi {
            let canonical_source = source_key(&gdi_display.source_name);
            if gdi_handles.insert(gdi_display.monitor_handle, ()).is_some()
                || gdi_sources.insert(canonical_source.clone(), ()).is_some()
            {
                return Err(unavailable("GDI reports duplicate active outputs"));
            }
            let dxgi_display = dxgi
                .remove(&gdi_display.monitor_handle)
                .ok_or_else(|| unavailable("GDI monitor is absent from attached DXGI outputs"))?;
            if source_key(&dxgi_display.source_name) != canonical_source
                || dxgi_display.rectangle != gdi_display.rectangle
            {
                return Err(unavailable("GDI and DXGI output metadata disagree"));
            }
            let ccd_display = ccd
                .remove(&canonical_source)
                .ok_or_else(|| unavailable("active output is absent from CCD paths"))?;
            if ccd_display.rectangle != gdi_display.rectangle {
                return Err(unavailable("CCD and capture output rectangles disagree"));
            }
            if target_ids
                .insert(ccd_display.target_id.to_ascii_lowercase(), ())
                .is_some()
            {
                return Err(unavailable(
                    "multiple active sources resolve to one physical target",
                ));
            }
            displays.push(VerifiedDisplay {
                source_name: gdi_display.source_name,
                target_id: ccd_display.target_id,
                monitor_handle: gdi_display.monitor_handle,
                rectangle: gdi_display.rectangle,
            });
        }
        if !dxgi.is_empty() || !ccd.is_empty() {
            return Err(unavailable(
                "Windows display enumerators left unmatched outputs",
            ));
        }
        Ok(Self { displays })
    }

    pub(crate) fn resolve(
        &self,
        selected: &SelectedOutput,
        expected_target: Option<&str>,
    ) -> Result<CaptureSelection, GeometryError> {
        selected
            .rectangle
            .validate()
            .map_err(|_| unavailable("selected output rectangle is invalid"))?;
        let display = self
            .displays
            .iter()
            .find(|display| {
                display.monitor_handle == selected.monitor_handle
                    && display
                        .source_name
                        .eq_ignore_ascii_case(&selected.source_name)
                    && display.rectangle == selected.rectangle
            })
            .ok_or_else(|| unavailable("selected output is absent from verified topology"))?;
        if expected_target.is_some_and(|expected| !display.target_id.eq_ignore_ascii_case(expected))
        {
            return Err(GeometryError::TargetChanged);
        }
        let geometry = CaptureGeometry::from_virtual_desktop(
            display.rectangle,
            self.displays.iter().map(|display| display.rectangle),
        )
        .map_err(|_| unavailable("verified virtual desktop is inconsistent"))?;
        Ok(CaptureSelection {
            geometry,
            target_id: display.target_id.clone(),
        })
    }
}

#[cfg(any(windows, test))]
fn unique_handles(
    displays: Vec<DisplaySnapshot>,
    duplicate_message: &'static str,
) -> Result<BTreeMap<usize, DisplaySnapshot>, GeometryError> {
    let mut result = BTreeMap::new();
    for display in displays {
        if result.insert(display.monitor_handle, display).is_some() {
            return Err(unavailable(duplicate_message));
        }
    }
    Ok(result)
}

#[cfg(any(windows, test))]
fn unique_sources(
    displays: Vec<CcdSnapshot>,
) -> Result<BTreeMap<String, CcdSnapshot>, GeometryError> {
    let mut result = BTreeMap::new();
    for display in displays {
        if display.target_id.is_empty()
            || result
                .insert(source_key(&display.source_name), display)
                .is_some()
        {
            return Err(unavailable(
                "CCD reports a missing target identity or ambiguous cloned source",
            ));
        }
    }
    Ok(result)
}

#[cfg(any(windows, test))]
fn source_key(source_name: &str) -> String {
    source_name.to_ascii_lowercase()
}

#[cfg(any(windows, test))]
pub(crate) fn unavailable(message: &'static str) -> GeometryError {
    GeometryError::Unavailable(message)
}

/// CCD encodes an SDR-white multiplier of 80 nits, scaled by 1000.
#[cfg(any(windows, test))]
pub(crate) fn reference_white_scale(value: u32) -> Result<f64, GeometryError> {
    if value == 0 || value > 125_000 {
        return Err(GeometryError::Unavailable(
            "native SDR white level is outside representable HDR luminance",
        ));
    }
    Ok(f64::from(value) / 1000.0)
}
