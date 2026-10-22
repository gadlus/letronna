//! What this computer has to run a model with: memory, graphics cards and
//! their memory, the processor, and whether it is on battery.
//!
//! It is looked at before any download, so a model is offered with an
//! honest verdict rather than tried and found too big. Apple's chips share
//! one memory, of which Metal says how much it can use well
//! (`recommendedMaxWorkingSetSize`: 12,124 MiB of 16 GB on an M5). On
//! Windows DXGI gives each card's own memory and what is free of it; on
//! Linux Vulkan does, where its loader is installed at all. The driver
//! version decides between CUDA builds, and Vulkan's presence between a
//! graphics card and the processor.
//!
//! Looking takes a moment (Vulkan loads every driver), so the answer is kept
//! in `models/machine.json` and looked at again only when it is over a
//! minute old and someone opens the picker or the Manage page. Once a
//! llama.cpp build is here, its own `--list-devices` names the device to use.

use super::catalog::{self, Entry};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// What llama.cpp computes with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Metal,
    /// CUDA 13.4: driver 580 or later, on Turing or later.
    Cuda13,
    /// CUDA 12.4 on Windows and 12.8 on Linux: driver 525 or later.
    Cuda12,
    Vulkan,
    Cpu,
}

impl Backend {
    /// Whether it is one of the CUDA builds, which need NVIDIA's runtime too.
    pub fn is_cuda(self) -> bool {
        matches!(self, Backend::Cuda13 | Backend::Cuda12)
    }

    /// Its name in folder names and files.
    pub fn name(self) -> &'static str {
        match self {
            Backend::Metal => "metal",
            Backend::Cuda13 => "cuda-13",
            Backend::Cuda12 => "cuda-12",
            Backend::Vulkan => "vulkan",
            Backend::Cpu => "cpu",
        }
    }

    /// What a person reads it as.
    pub fn shown(self) -> &'static str {
        match self {
            Backend::Metal => "Metal",
            Backend::Cuda13 => "CUDA 13",
            Backend::Cuda12 => "CUDA 12",
            Backend::Vulkan => "Vulkan",
            Backend::Cpu => "the processor",
        }
    }

    /// The prefix llama.cpp gives this backend's devices.
    fn devices(self) -> Option<&'static str> {
        match self {
            Backend::Metal => Some("MTL"),
            Backend::Cuda13 | Backend::Cuda12 => Some("CUDA"),
            Backend::Vulkan => Some("Vulkan"),
            Backend::Cpu => None,
        }
    }
}

/// Who made a graphics chip, from its PCI vendor id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    Apple,
    Nvidia,
    Amd,
    Intel,
    Other,
}

impl Vendor {
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn of(id: u32) -> Vendor {
        match id {
            0x106B => Vendor::Apple,
            0x10DE => Vendor::Nvidia,
            0x1002 | 0x1022 => Vendor::Amd,
            0x8086 => Vendor::Intel,
            _ => Vendor::Other,
        }
    }
}

/// Where a graphics chip keeps what it works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Apple's: one memory for the processor and the graphics.
    Unified,
    /// A card with memory of its own.
    Discrete,
    /// Built into the processor, borrowing the system's memory.
    Integrated,
}

/// A graphics chip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    pub vendor: Vendor,
    pub kind: Kind,
    /// Its own memory in bytes; for Apple's, what Metal can use well.
    pub total: u64,
    /// What is free of it now.
    pub free: u64,
    /// The driver's version, as NVIDIA numbers it ("580.88").
    #[serde(default)]
    pub driver: Option<String>,
}

/// This computer, as far as running a model goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    /// As `std::env::consts::OS`: "macos", "windows" or "linux".
    pub os: String,
    /// The processor's own, so "aarch64" for Windows on ARM running the x64 app.
    pub arch: String,
    pub cpu: String,
    pub avx2: bool,
    pub avx512: bool,
    pub ram_total: u64,
    pub ram_free: u64,
    pub gpus: Vec<Gpu>,
    /// The newest Vulkan the loader and a device both have, or none.
    pub vulkan: Option<(u32, u32)>,
    pub on_battery: bool,
    pub low_power: bool,
    /// When it was looked at, in seconds since 1970.
    pub at: u64,
}

impl Machine {
    /// Whether this is an Apple chip, whose graphics share its memory.
    pub fn unified(&self) -> bool {
        self.gpus.iter().any(|g| g.kind == Kind::Unified)
    }
}

/// How a model would run here: the backend, the graphics chip it uses (an
/// index into `Machine::gpus`), and something to tell the user, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub backend: Backend,
    pub gpu: Option<usize>,
    pub note: Option<&'static str>,
}

/// Said when an NVIDIA driver is too old for either CUDA build and Vulkan,
/// or when Windows has no Vulkan at all.
pub const OLD_DRIVER: &str = "Update your graphics driver for faster answers.";
/// Said on Linux with no Vulkan loader.
pub const NO_VULKAN: &str = "Install your distribution's Vulkan driver to use the graphics card.";

/// The oldest NVIDIA driver each CUDA build runs on.
const CUDA_13_DRIVER: u32 = 580;
const CUDA_12_DRIVER: u32 = 525;

/// How to run `entry` on `machine`.
///
/// Apple's chips use Metal. On NVIDIA, Vulkan writes about as fast as CUDA
/// and is a twentieth of the download, so it is the default; but Bonsai's
/// 9B and 27B use Hadamard blocks above 512, which Vulkan takes a slow path
/// for, so those get CUDA where the driver allows. AMD and Intel use
/// Vulkan, which on RDNA3 is faster than ROCm. A discrete card is chosen
/// over the processor's own graphics, and among cards the one with the most
/// memory free.
pub fn pick(machine: &Machine, entry: &Entry) -> Choice {
    let cpu = |note| Choice {
        backend: Backend::Cpu,
        gpu: None,
        note,
    };
    let has = |backend| catalog::runtime(&machine.os, &machine.arch, backend).is_some();
    if let Some(i) = machine.gpus.iter().position(|g| g.kind == Kind::Unified) {
        if has(Backend::Metal) {
            return Choice {
                backend: Backend::Metal,
                gpu: Some(i),
                note: None,
            };
        }
        return cpu(None);
    }
    let discrete = machine.gpus.iter().any(|g| g.kind == Kind::Discrete);
    let Some((i, gpu)) = machine
        .gpus
        .iter()
        .enumerate()
        .filter(|(_, g)| !discrete || g.kind == Kind::Discrete)
        .max_by_key(|(_, g)| g.free)
    else {
        return cpu(None);
    };
    let vulkan = machine.vulkan.is_some_and(|v| v >= (1, 2)) && has(Backend::Vulkan);
    let on = |backend| Choice {
        backend,
        gpu: Some(i),
        note: None,
    };
    if gpu.vendor == Vendor::Nvidia {
        let driver = gpu.driver.as_deref().and_then(driver_major).unwrap_or(0);
        let cuda = if driver >= CUDA_13_DRIVER && turing_or_later(&gpu.name) && has(Backend::Cuda13)
        {
            Some(Backend::Cuda13)
        } else if driver >= CUDA_12_DRIVER && has(Backend::Cuda12) {
            Some(Backend::Cuda12)
        } else {
            None
        };
        let hadamard = matches!(entry.id, "bonsai-2-9b" | "bonsai-2-27b");
        return match (cuda, vulkan) {
            (Some(cuda), false) => on(cuda),
            (Some(cuda), true) if hadamard => on(cuda),
            (_, true) => on(Backend::Vulkan),
            (None, false) => cpu(Some(OLD_DRIVER)),
        };
    }
    if vulkan {
        return on(Backend::Vulkan);
    }
    cpu(Some(if machine.os == "linux" {
        NO_VULKAN
    } else {
        OLD_DRIVER
    }))
}

/// The major number of an NVIDIA driver version: 580 for "580.88".
fn driver_major(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse().ok()
}

/// Whether an NVIDIA chip is Turing (compute 7.5) or later, which CUDA 13
/// needs: every RTX, the GTX 16s, and the data-centre parts from T4 on.
fn turing_or_later(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let name = name.trim_start_matches("nvidia ").trim();
    if name.contains("rtx") || name.contains("gtx 16") || name.contains(" t4") || name == "t4" {
        return true;
    }
    let part = name
        .trim_start_matches("tesla ")
        .trim_start_matches("quadro ");
    let mut chars = part.chars();
    matches!(chars.next(), Some('a' | 'h' | 'l' | 'b'))
        && chars.next().is_some_and(|c| c.is_ascii_digit())
}

/// One of the devices a llama.cpp build lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// What `--device` takes: "MTL0", "Vulkan1", "CUDA0".
    pub name: String,
    pub description: String,
    pub total_mib: u64,
    pub free_mib: u64,
}

/// The devices in `llama-server --list-devices`' output, each line of which
/// reads `  Vulkan0: NVIDIA GeForce RTX 4090 (24564 MiB, 23000 MiB free)`.
pub fn parse_devices(output: &str) -> Vec<Device> {
    output
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.trim().split_once(": ")?;
            let open = rest.rfind(" (")?;
            let (description, sizes) = (&rest[..open], rest[open + 2..].strip_suffix(')')?);
            let (total, free) = sizes.split_once(", ")?;
            let total_mib = total.strip_suffix(" MiB")?.trim().parse().ok()?;
            let free_mib = free.strip_suffix(" MiB free")?.trim().parse().ok()?;
            (!name.contains(' ') && total_mib > 0).then(|| Device {
                name: name.to_owned(),
                description: description.trim().to_owned(),
                total_mib,
                free_mib,
            })
        })
        .collect()
}

/// The device to pass `--device` for `gpu` on `backend`: the one of its
/// name, else the one with the most memory free. Vulkan's numbering is not
/// DXGI's, so a number from one never names a device in the other.
pub fn device_for(devices: &[Device], backend: Backend, gpu: Option<&Gpu>) -> Option<String> {
    let prefix = backend.devices()?;
    let ours: Vec<&Device> = devices
        .iter()
        .filter(|d| {
            d.name
                .strip_prefix(prefix)
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    let named = gpu.and_then(|gpu| {
        ours.iter()
            .find(|d| d.description.eq_ignore_ascii_case(gpu.name.trim()))
            .or_else(|| {
                let name = gpu.name.to_ascii_lowercase();
                ours.iter()
                    .find(|d| name.contains(&d.description.to_ascii_lowercase()))
            })
    });
    named
        .or_else(|| ours.iter().max_by_key(|d| d.free_mib))
        .map(|d| d.name.clone())
}

/// How long a look at this computer is trusted.
const FRESH: u64 = 60;

fn cache() -> PathBuf {
    crate::chat::base_dir().join("models").join("machine.json")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// This computer: as last looked at, if that was under a minute ago, else
/// looked at again. It can take a second, so never call it on a UI thread.
pub fn now() -> Machine {
    if let Some(machine) = cached().filter(|m| unix_now().saturating_sub(m.at) < FRESH) {
        return machine;
    }
    let machine = detect();
    if let Ok(text) = serde_json::to_string_pretty(&machine) {
        let path = cache();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
    machine
}

/// This computer as last looked at, however long ago, without looking.
pub fn cached() -> Option<Machine> {
    serde_json::from_slice(&std::fs::read(cache()).ok()?).ok()
}

/// Looks at this computer.
pub fn detect() -> Machine {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    system.refresh_cpu_list(sysinfo::CpuRefreshKind::new());
    let cpu = system
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_owned())
        .unwrap_or_default();
    #[cfg(target_arch = "x86_64")]
    let (avx2, avx512) = (
        std::arch::is_x86_feature_detected!("avx2"),
        std::arch::is_x86_feature_detected!("avx512f"),
    );
    #[cfg(not(target_arch = "x86_64"))]
    let (avx2, avx512) = (false, false);
    let (gpus, vulkan) = graphics();
    let (on_battery, low_power) = power();
    Machine {
        os: std::env::consts::OS.to_owned(),
        arch: arch(),
        cpu,
        avx2,
        avx512,
        ram_total: system.total_memory(),
        ram_free: available(&system),
        gpus,
        vulkan,
        on_battery,
        low_power,
        at: unix_now(),
    }
}

/// The memory free right now, for the check made as a model loads.
pub fn ram_free() -> u64 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    available(&system)
}

#[cfg(not(target_os = "macos"))]
fn available(system: &sysinfo::System) -> u64 {
    system.available_memory()
}

/// macOS keeps most memory in use, compressed or cached, and gives it up
/// as asked; its own measure of what is free is the share the kernel
/// reports, which Activity Monitor's pressure graph follows. sysinfo's
/// count of free pages read 1.3 GB on a 16 GB Mac the kernel had at 52%.
#[cfg(target_os = "macos")]
fn available(system: &sysinfo::System) -> u64 {
    let mut level: i32 = 0;
    let mut len = std::mem::size_of::<i32>();
    // SAFETY: the name is a NUL-terminated literal and the out-pointer is
    // to a local of the size given.
    let ok = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_level".as_ptr(),
            (&mut level as *mut i32).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    if !ok || !(0..=100).contains(&level) {
        return system.available_memory();
    }
    system.total_memory() / 100 * level as u64
}

#[cfg(not(windows))]
fn arch() -> String {
    std::env::consts::ARCH.to_owned()
}

#[cfg(windows)]
fn arch() -> String {
    use windows_sys::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_ARM64;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
    let (mut process, mut native) = (0, 0);
    // SAFETY: both pointers are to live locals, and the handle is the
    // pseudo-handle of this process, which needs no closing.
    let ok = unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } != 0;
    // The x64 app runs emulated on Windows on ARM, where the arm64 runtime
    // is the one to fetch.
    if ok && native == IMAGE_FILE_MACHINE_ARM64 {
        return "aarch64".to_owned();
    }
    std::env::consts::ARCH.to_owned()
}

#[cfg(target_os = "macos")]
fn graphics() -> (Vec<Gpu>, Option<(u32, u32)>) {
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};
    let Some(device) = MTLCreateSystemDefaultDevice() else {
        return (Vec::new(), None);
    };
    let total = device.recommendedMaxWorkingSetSize();
    let gpu = Gpu {
        name: device.name().to_string(),
        vendor: Vendor::Apple,
        kind: if device.hasUnifiedMemory() {
            Kind::Unified
        } else {
            Kind::Discrete
        },
        total,
        free: total,
        driver: None,
    };
    (vec![gpu], None)
}

#[cfg(windows)]
fn graphics() -> (Vec<Gpu>, Option<(u32, u32)>) {
    let (_, vulkan) = vulkan::devices();
    (dxgi::adapters(), vulkan)
}

#[cfg(target_os = "linux")]
fn graphics() -> (Vec<Gpu>, Option<(u32, u32)>) {
    let (mut gpus, vulkan) = vulkan::devices();
    let driver = std::fs::read_to_string("/proc/driver/nvidia/version")
        .ok()
        .and_then(|text| nvidia_version(&text));
    for gpu in gpus.iter_mut().filter(|g| g.vendor == Vendor::Nvidia) {
        gpu.driver.clone_from(&driver);
    }
    (gpus, vulkan)
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
fn graphics() -> (Vec<Gpu>, Option<(u32, u32)>) {
    (Vec::new(), None)
}

/// The driver version in `/proc/driver/nvidia/version`, whose first line
/// reads `NVRM version: NVIDIA UNIX x86_64 Kernel Module  580.82.07  …`.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn nvidia_version(text: &str) -> Option<String> {
    text.lines().next()?.split_whitespace().find_map(|word| {
        let mut parts = word.split('.');
        let major = parts.next()?;
        let minor = parts.next()?;
        (major.len() == 3
            && major.bytes().all(|b| b.is_ascii_digit())
            && minor.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("{major}.{minor}"))
    })
}

/// The version DXGI reads off NVIDIA's user-mode driver, 32.0.15.8088, as
/// NVIDIA numbers it, 580.88: the last digit of the third part and the
/// fourth part.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn nvidia_from_umd(umd: i64) -> String {
    let third = (umd >> 16) & 0xFFFF;
    let fourth = umd & 0xFFFF;
    let digits = (third % 10) * 10_000 + fourth;
    format!("{}.{:02}", digits / 100, digits % 100)
}

#[cfg(windows)]
mod dxgi {
    use super::{Gpu, Kind, Vendor, nvidia_from_umd};
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
        DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIDevice, IDXGIFactory1,
    };
    use windows::core::Interface;

    /// Below this much memory of its own, an AMD or Intel chip is the one
    /// built into the processor (AMD's report 512 MB).
    const OWN_MEMORY: usize = 1 << 30;
    /// Microsoft's Basic Render Driver, which draws in software.
    const MICROSOFT: u32 = 0x1414;

    /// The graphics adapters DXGI lists, without software ones.
    pub fn adapters() -> Vec<Gpu> {
        // SAFETY: DXGI's factory and adapters are COM objects the windows
        // crate counts references for; every out-pointer is to a local.
        unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
                return Vec::new();
            };
            let mut found = Vec::new();
            let mut i = 0;
            while let Ok(adapter) = factory.EnumAdapters1(i) {
                i += 1;
                let Ok(desc) = adapter.GetDesc1() else {
                    continue;
                };
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0
                    || desc.VendorId == MICROSOFT
                {
                    continue;
                }
                let vendor = Vendor::of(desc.VendorId);
                let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
                let name = String::from_utf16_lossy(&desc.Description[..len]);
                let integrated = matches!(vendor, Vendor::Intel | Vendor::Amd)
                    && desc.DedicatedVideoMemory < OWN_MEMORY;
                let total = desc.DedicatedVideoMemory as u64;
                let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                let free = adapter
                    .cast::<IDXGIAdapter3>()
                    .ok()
                    .and_then(|a| {
                        a.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info)
                            .ok()
                    })
                    .map_or(total, |()| info.Budget.saturating_sub(info.CurrentUsage));
                let driver = (vendor == Vendor::Nvidia)
                    .then(|| adapter.CheckInterfaceSupport(&IDXGIDevice::IID).ok())
                    .flatten()
                    .map(nvidia_from_umd);
                found.push(Gpu {
                    name,
                    vendor,
                    kind: if integrated {
                        Kind::Integrated
                    } else {
                        Kind::Discrete
                    },
                    total,
                    free,
                    driver,
                });
            }
            found
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
mod vulkan {
    use super::{Gpu, Kind, Vendor};
    use ash::vk;

    /// The graphics devices Vulkan lists, and the newest Vulkan the loader
    /// and a device both have; nothing without a loader.
    pub fn devices() -> (Vec<Gpu>, Option<(u32, u32)>) {
        // SAFETY: the instance is made, used and destroyed here alone, and
        // every structure handed to it lives until the call returns.
        unsafe {
            let Ok(entry) = ash::Entry::load() else {
                return (Vec::new(), None);
            };
            let loader = entry
                .try_enumerate_instance_version()
                .ok()
                .flatten()
                .unwrap_or(vk::API_VERSION_1_0);
            let app = vk::ApplicationInfo::default().api_version(loader.min(vk::API_VERSION_1_1));
            let info = vk::InstanceCreateInfo::default().application_info(&app);
            let Ok(instance) = entry.create_instance(&info, None) else {
                return (Vec::new(), None);
            };
            let mut found = Vec::new();
            let mut newest = None;
            for device in instance.enumerate_physical_devices().unwrap_or_default() {
                let props = instance.get_physical_device_properties(device);
                let kind = match props.device_type {
                    vk::PhysicalDeviceType::DISCRETE_GPU => Kind::Discrete,
                    vk::PhysicalDeviceType::INTEGRATED_GPU => Kind::Integrated,
                    // llvmpipe and the like draw on the processor.
                    _ => continue,
                };
                let version = props.api_version.min(loader);
                let version = (
                    vk::api_version_major(version),
                    vk::api_version_minor(version),
                );
                newest = newest.max(Some(version));
                let name = props
                    .device_name_as_c_str()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let (total, free) = memory(&instance, device, loader);
                found.push(Gpu {
                    name,
                    vendor: Vendor::of(props.vendor_id),
                    kind,
                    total,
                    free,
                    driver: None,
                });
            }
            instance.destroy_instance(None);
            (found, newest)
        }
    }

    /// A device's own memory and what its driver says is free of it, where
    /// it has VK_EXT_memory_budget; else all of it counts as free.
    unsafe fn memory(
        instance: &ash::Instance,
        device: vk::PhysicalDevice,
        loader: u32,
    ) -> (u64, u64) {
        // SAFETY: as in `devices`, whose instance this is.
        unsafe {
            let props = instance.get_physical_device_memory_properties(device);
            let local: Vec<usize> = (0..props.memory_heap_count as usize)
                .filter(|&i| {
                    props.memory_heaps[i]
                        .flags
                        .contains(vk::MemoryHeapFlags::DEVICE_LOCAL)
                })
                .collect();
            let total: u64 = local.iter().map(|&i| props.memory_heaps[i].size).sum();
            let budget = loader >= vk::API_VERSION_1_1
                && instance
                    .enumerate_device_extension_properties(device)
                    .unwrap_or_default()
                    .iter()
                    .any(|e| e.extension_name_as_c_str() == Ok(vk::EXT_MEMORY_BUDGET_NAME));
            if !budget {
                return (total, total);
            }
            let mut budgets = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
            let mut both = vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budgets);
            instance.get_physical_device_memory_properties2(device, &mut both);
            let free = local
                .iter()
                .map(|&i| budgets.heap_budget[i].saturating_sub(budgets.heap_usage[i]))
                .sum();
            (total, free)
        }
    }
}

/// Whether this computer runs on its battery, and whether it is saving power.
#[cfg(target_os = "macos")]
fn power() -> (bool, bool) {
    use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFStringRef;
    }

    // SAFETY: the snapshot is released once read; the source type is
    // borrowed from it (the Get rule) and copied out before that.
    let on_battery = unsafe {
        let snapshot = IOPSCopyPowerSourcesInfo();
        if snapshot.is_null() {
            false
        } else {
            let kind = IOPSGetProvidingPowerSourceType(snapshot);
            let battery = !kind.is_null() && CFString::wrap_under_get_rule(kind) == "Battery Power";
            CFRelease(snapshot);
            battery
        }
    };
    let low_power = objc2_foundation::NSProcessInfo::processInfo().isLowPowerModeEnabled();
    (on_battery, low_power)
}

#[cfg(windows)]
fn power() -> (bool, bool) {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // SAFETY: the status is a plain struct the call fills in.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return (false, false);
    }
    // 0 is on battery, 1 on mains, 255 unknown; the status flag is 1 while
    // battery saver is on.
    (status.ACLineStatus == 0, status.SystemStatusFlag == 1)
}

#[cfg(target_os = "linux")]
fn power() -> (bool, bool) {
    let read = |path: std::path::PathBuf| {
        std::fs::read_to_string(path)
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let mut mains = None;
    let mut battery = false;
    if let Ok(supplies) = std::fs::read_dir("/sys/class/power_supply") {
        for supply in supplies.flatten() {
            match read(supply.path().join("type")).as_str() {
                "Mains" => {
                    mains =
                        Some(mains.unwrap_or(false) || read(supply.path().join("online")) == "1")
                }
                "Battery" => battery = true,
                _ => {}
            }
        }
    }
    let profile = read("/sys/firmware/acpi/platform_profile".into());
    (
        battery && mains == Some(false),
        profile == "low-power" || profile == "quiet",
    )
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
fn power() -> (bool, bool) {
    (false, false)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    fn entry(id: &str) -> &'static Entry {
        catalog::entry(id).unwrap()
    }

    /// A machine for the tests, with nothing but its memory.
    pub(crate) fn bare(os: &str, arch: &str, ram: u64) -> Machine {
        Machine {
            os: os.into(),
            arch: arch.into(),
            cpu: String::new(),
            avx2: arch == "x86_64",
            avx512: false,
            ram_total: ram,
            ram_free: ram / 2,
            gpus: Vec::new(),
            vulkan: None,
            on_battery: false,
            low_power: false,
            at: 0,
        }
    }

    pub(crate) fn apple(name: &str, ram: u64, working_set: u64) -> Machine {
        Machine {
            cpu: name.into(),
            gpus: vec![Gpu {
                name: name.into(),
                vendor: Vendor::Apple,
                kind: Kind::Unified,
                total: working_set,
                free: working_set,
                driver: None,
            }],
            ..bare("macos", "aarch64", ram)
        }
    }

    pub(crate) fn card(
        name: &str,
        vendor: Vendor,
        kind: Kind,
        memory: u64,
        driver: Option<&str>,
    ) -> Gpu {
        Gpu {
            name: name.into(),
            vendor,
            kind,
            total: memory,
            free: memory - GB / 2,
            driver: driver.map(Into::into),
        }
    }

    pub(crate) fn pc(os: &str, ram: u64, gpus: Vec<Gpu>) -> Machine {
        Machine {
            gpus,
            vulkan: Some((1, 3)),
            ..bare(os, "x86_64", ram)
        }
    }

    #[test]
    fn an_apple_chip_runs_on_metal() {
        let m5 = apple("Apple M5", 16 * GB, 12_124 << 20);
        let choice = pick(&m5, entry("bonsai-2-9b"));
        assert_eq!(
            (choice.backend, choice.gpu, choice.note),
            (Backend::Metal, Some(0), None)
        );
    }

    #[test]
    fn the_card_with_the_most_free_memory_wins_over_the_processors_graphics() {
        let machine = pc(
            "windows",
            32 * GB,
            vec![
                card(
                    "Intel(R) UHD Graphics 770",
                    Vendor::Intel,
                    Kind::Integrated,
                    16 * GB,
                    None,
                ),
                card(
                    "NVIDIA GeForce RTX 4060",
                    Vendor::Nvidia,
                    Kind::Discrete,
                    8 * GB,
                    Some("580.88"),
                ),
            ],
        );
        let choice = pick(&machine, entry("bonsai-2-4b"));
        assert_eq!((choice.backend, choice.gpu), (Backend::Vulkan, Some(1)));
    }

    #[test]
    fn a_card_unplugged_since_the_last_look_leaves_the_processors_graphics() {
        let mut machine = pc(
            "windows",
            32 * GB,
            vec![
                card(
                    "Intel(R) Iris(R) Xe Graphics",
                    Vendor::Intel,
                    Kind::Integrated,
                    16 * GB,
                    None,
                ),
                card(
                    "NVIDIA GeForce RTX 4090",
                    Vendor::Nvidia,
                    Kind::Discrete,
                    24 * GB,
                    Some("580.88"),
                ),
            ],
        );
        assert_eq!(pick(&machine, entry("bonsai-2-4b")).gpu, Some(1));
        machine.gpus.pop();
        let choice = pick(&machine, entry("bonsai-2-4b"));
        assert_eq!((choice.backend, choice.gpu), (Backend::Vulkan, Some(0)));
    }

    #[test]
    fn bonsais_big_blocks_take_cuda_where_the_driver_allows() {
        let machine = pc(
            "windows",
            32 * GB,
            vec![card(
                "NVIDIA GeForce RTX 4070",
                Vendor::Nvidia,
                Kind::Discrete,
                12 * GB,
                Some("581.15"),
            )],
        );
        assert_eq!(
            pick(&machine, entry("bonsai-2-9b")).backend,
            Backend::Cuda13
        );
        assert_eq!(
            pick(&machine, entry("bonsai-2-27b")).backend,
            Backend::Cuda13
        );
        assert_eq!(
            pick(&machine, entry("bonsai-2-4b")).backend,
            Backend::Vulkan
        );
        assert_eq!(
            pick(&machine, entry("qwen-3.5-9b")).backend,
            Backend::Vulkan
        );
    }

    #[test]
    fn an_nvidia_driver_picks_the_cuda_it_can_run() {
        let with = |driver: &str| {
            pc(
                "windows",
                32 * GB,
                vec![card(
                    "NVIDIA GeForce RTX 3080",
                    Vendor::Nvidia,
                    Kind::Discrete,
                    10 * GB,
                    Some(driver),
                )],
            )
        };
        let bonsai = entry("bonsai-2-9b");
        assert_eq!(pick(&with("575.64"), bonsai).backend, Backend::Cuda12);
        assert_eq!(pick(&with("520.10"), bonsai).backend, Backend::Vulkan);
        let mut old = with("520.10");
        old.vulkan = None;
        let choice = pick(&old, bonsai);
        assert_eq!(
            (choice.backend, choice.note),
            (Backend::Cpu, Some(OLD_DRIVER))
        );
    }

    #[test]
    fn a_card_older_than_turing_never_gets_cuda_13() {
        let machine = pc(
            "linux",
            32 * GB,
            vec![card(
                "NVIDIA GeForce GTX 1080 Ti",
                Vendor::Nvidia,
                Kind::Discrete,
                11 * GB,
                Some("580.82"),
            )],
        );
        assert_eq!(
            pick(&machine, entry("bonsai-2-9b")).backend,
            Backend::Cuda12
        );
        assert!(turing_or_later("NVIDIA GeForce GTX 1660 SUPER"));
        assert!(turing_or_later("NVIDIA A100-SXM4-40GB"));
        assert!(turing_or_later("Tesla T4"));
        assert!(!turing_or_later("Tesla V100-PCIE-16GB"));
        assert!(!turing_or_later("NVIDIA TITAN Xp"));
    }

    #[test]
    fn linux_without_a_vulkan_loader_runs_on_the_processor_and_says_how_to_fix_it() {
        let mut machine = pc(
            "linux",
            32 * GB,
            vec![card(
                "AMD Radeon RX 7800 XT",
                Vendor::Amd,
                Kind::Discrete,
                16 * GB,
                None,
            )],
        );
        assert_eq!(
            pick(&machine, entry("bonsai-2-4b")).backend,
            Backend::Vulkan
        );
        machine.vulkan = None;
        machine.gpus.clear();
        let choice = pick(&machine, entry("bonsai-2-4b"));
        assert_eq!((choice.backend, choice.note), (Backend::Cpu, None));
        machine.gpus = vec![card(
            "AMD Radeon RX 7800 XT",
            Vendor::Amd,
            Kind::Discrete,
            16 * GB,
            None,
        )];
        let choice = pick(&machine, entry("bonsai-2-4b"));
        assert_eq!(
            (choice.backend, choice.note),
            (Backend::Cpu, Some(NO_VULKAN))
        );
    }

    #[test]
    fn a_vulkan_older_than_1_2_counts_as_none() {
        let mut machine = pc(
            "windows",
            16 * GB,
            vec![card(
                "Intel(R) Arc(TM) A770 Graphics",
                Vendor::Intel,
                Kind::Discrete,
                16 * GB,
                None,
            )],
        );
        machine.vulkan = Some((1, 1));
        let choice = pick(&machine, entry("qwen-3.5-9b"));
        assert_eq!(
            (choice.backend, choice.note),
            (Backend::Cpu, Some(OLD_DRIVER))
        );
    }

    #[test]
    fn windows_on_arm_takes_the_arm64_runtime() {
        let machine = Machine {
            gpus: vec![card(
                "Qualcomm(R) Adreno(TM) X1-85 GPU",
                Vendor::Other,
                Kind::Integrated,
                GB,
                None,
            )],
            vulkan: Some((1, 3)),
            ..bare("windows", "aarch64", 16 * GB)
        };
        let choice = pick(&machine, entry("bonsai-2-4b"));
        assert_eq!(choice.backend, Backend::Vulkan);
        let runtime = catalog::runtime(&machine.os, &machine.arch, choice.backend).unwrap();
        assert!(runtime.archive.name.contains("arm64"));
    }

    #[test]
    fn being_on_battery_changes_how_it_runs_nothing() {
        let mut m5 = apple("Apple M5", 16 * GB, 12_124 << 20);
        let plugged = pick(&m5, entry("bonsai-2-4b"));
        m5.on_battery = true;
        m5.low_power = true;
        assert_eq!(pick(&m5, entry("bonsai-2-4b")), plugged);
    }

    #[test]
    fn the_devices_a_build_lists_are_read_and_matched_to_the_card() {
        let listed = "load_backend: loaded RPC backend\nAvailable devices:\n  \
            Vulkan0: Intel(R) UHD Graphics 770 (16207 MiB, 15000 MiB free)\n  \
            Vulkan1: NVIDIA GeForce RTX 4060 (8188 MiB, 7600 MiB free)\n  \
            BLAS: Accelerate (0 MiB, 0 MiB free)\n";
        let devices = parse_devices(listed);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[1].name, "Vulkan1");
        assert_eq!((devices[1].total_mib, devices[1].free_mib), (8188, 7600));
        let rtx = card(
            "NVIDIA GeForce RTX 4060",
            Vendor::Nvidia,
            Kind::Discrete,
            8 * GB,
            None,
        );
        assert_eq!(
            device_for(&devices, Backend::Vulkan, Some(&rtx)).as_deref(),
            Some("Vulkan1")
        );
        let unknown = card(
            "Something else",
            Vendor::Other,
            Kind::Discrete,
            8 * GB,
            None,
        );
        assert_eq!(
            device_for(&devices, Backend::Vulkan, Some(&unknown)).as_deref(),
            Some("Vulkan0")
        );
        assert_eq!(device_for(&devices, Backend::Cpu, Some(&rtx)), None);
        let m5 =
            parse_devices("Available devices:\n  MTL0: Apple M5 (12124 MiB, 12123 MiB free)\n");
        assert_eq!(
            device_for(&m5, Backend::Metal, None).as_deref(),
            Some("MTL0")
        );
    }

    #[test]
    fn nvidias_driver_version_is_read_from_linux_and_from_dxgi() {
        let proc = "NVRM version: NVIDIA UNIX x86_64 Kernel Module  580.82.07  Wed Aug 13 2025\n";
        assert_eq!(nvidia_version(proc).as_deref(), Some("580.82"));
        let open =
            "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  575.64.03  Release Build\n";
        assert_eq!(nvidia_version(open).as_deref(), Some("575.64"));
        // 32.0.15.8088
        let umd = (32i64 << 48) | (15 << 16) | 8088;
        assert_eq!(nvidia_from_umd(umd), "580.88");
        assert_eq!(driver_major("580.88"), Some(580));
    }

    #[test]
    fn a_look_at_this_computer_finds_its_memory() {
        let machine = detect();
        assert!(machine.ram_total > 1_000_000_000);
        assert!(machine.ram_free <= machine.ram_total);
        assert_eq!(machine.os, std::env::consts::OS);
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert!(machine.unified(), "{machine:?}");
        }
    }
}
