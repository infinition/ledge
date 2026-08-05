//! Lecture GPU + VRAM via NVML (NVIDIA). Dégrade silencieusement si absent.

use nvml_wrapper::Nvml;

pub struct Gpu {
    nvml: Option<Nvml>,
}

impl Gpu {
    pub fn new() -> Self {
        Gpu { nvml: Nvml::init().ok() }
    }

    /// (utilisation GPU %, VRAM utilisée Go, VRAM totale Go)
    pub fn read(&self) -> Option<(u32, f64, f64)> {
        let nvml = self.nvml.as_ref()?;
        let dev = nvml.device_by_index(0).ok()?;
        let util = dev.utilization_rates().ok()?.gpu;
        let mem = dev.memory_info().ok()?;
        let gib = 1_073_741_824.0;
        Some((util, mem.used as f64 / gib, mem.total as f64 / gib))
    }
}
