//! Will a model run here, how fast, and which one to suggest: shown before
//! anything is downloaded, so nobody waits for gigabytes that cannot run.
//!
//! Memory is the file, the KV cache for the context it is started with,
//! the recurrent state, and a margin for the runtime: 0.5 GB, growing with
//! the file to 1 GB. Speed is bound by memory bandwidth -- each token reads
//! every active weight once -- so tokens a second are bandwidth over active
//! bytes, times an efficiency between 0.35 and 0.60 (an M5 measured 0.44 to
//! 0.56). A model too big for the graphics card's memory spills to the
//! system's and runs at the rate of the two together. These are estimates;
//! once a model is here its first answer is timed and that is shown instead.

use super::catalog::{self, CATALOG, Entry, Pack, Tools};
use super::machine::{Backend, Choice, Kind, Machine, Vendor, pick};

const GB: f64 = 1e9;
const GIB: f64 = 1_073_741_824.0;

/// The context a model starts with.
pub const CONTEXT: u64 = 16_384;
/// An agent's, where it fits: its tools alone take a few thousand tokens.
pub const AGENT_CONTEXT: u64 = 32_768;
/// The least it is started with: the request keeps 6,000 tokens for the
/// conversation and a summary reads up to 12,000.
pub const LEAST_CONTEXT: u64 = 12_288;

/// The share of the system's memory a model may take, leaving the rest for
/// the system and the apps open beside it.
const SYSTEM_SHARE: f64 = 0.70;
/// What a graphics card keeps back for the display and the runtime.
const CARD_RESERVE: f64 = 1.0 * GB;
/// What reading photos costs beyond the vision pack itself.
const PHOTOS_MARGIN: f64 = 0.3 * GB;
/// The efficiency bounds of the speed estimate.
const EFFICIENCY: (f64, f64) = (0.35, 0.60);
/// The fewest tokens a second that still read as running well.
const WELL: f64 = 8.0;
/// The fewest for a model to be suggested.
const BRISK: f64 = 15.0;

/// Whether and how well a model runs here.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// All of it fits in the fast memory, at a fair speed.
    RunsWell {
        lo: f64,
        hi: f64,
    },
    /// It fits, but spills or crawls.
    Slower {
        lo: f64,
        hi: f64,
    },
    NotEnoughMemory {
        need: u64,
        total: u64,
        usable: u64,
    },
    NotSupported(String),
}

impl Verdict {
    /// Whether it can run here at all.
    pub fn runs(&self) -> bool {
        matches!(self, Verdict::RunsWell { .. } | Verdict::Slower { .. })
    }

    /// Its badge: "Runs well · 44–76 tok/s".
    pub fn label(&self) -> String {
        match self {
            Verdict::RunsWell { lo, hi } => format!("Runs well · {}", speed(*lo, *hi)),
            Verdict::Slower { lo, hi } => format!("Runs, slower · {}", speed(*lo, *hi)),
            Verdict::NotEnoughMemory { .. } => "Not enough memory".into(),
            Verdict::NotSupported(_) => "Not supported".into(),
        }
    }

    /// Why it cannot run, in a sentence; nothing when it can.
    pub fn why(&self) -> Option<String> {
        match self {
            Verdict::NotEnoughMemory {
                need,
                total,
                usable,
            } => Some(format!(
                "Needs {} · this computer has {} ({} usable).",
                memory(*need),
                memory(*total),
                memory(*usable)
            )),
            Verdict::NotSupported(why) => Some(why.clone()),
            _ => None,
        }
    }
}

/// A speed range as shown: "6–9 tok/s".
pub fn speed(lo: f64, hi: f64) -> String {
    let (lo, hi) = (lo.floor().max(1.0), hi.round().max(1.0));
    if lo >= hi {
        return format!("{hi} tok/s");
    }
    format!("{lo}–{hi} tok/s")
}

/// An amount of memory as people know it: "16 GB" for 16 GiB, as a system's
/// own panels count it.
pub fn memory(bytes: u64) -> String {
    format!("{} GB", (bytes as f64 / GIB).round().max(1.0))
}

/// A download's size, counted as the sites that offer it count: "1.2 GB",
/// "367 MB", "38 GB".
pub fn size(bytes: u64) -> String {
    let gb = bytes as f64 / GB;
    if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", (bytes as f64 / 1e6).max(1.0))
    }
}

/// How a model would run here, all told.
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    pub verdict: Verdict,
    pub choice: Choice,
    /// Whether the smaller of its files is the one to take.
    pub smaller: bool,
    /// The context it would be started with.
    pub context: u64,
    /// Memory it would take, in bytes.
    pub need: u64,
    /// Something to say beside the verdict.
    pub note: Option<&'static str>,
}

impl Fit {
    /// The file it would run.
    pub fn pack<'a>(&self, entry: &'a Entry) -> &'a Pack {
        entry.pack(self.smaller)
    }
}

/// Said beside a verdict on a laptop running on its battery.
pub const ON_BATTERY: &str = "On battery: expect it slower.";

/// How `entry` would run on `machine`, with photos or without.
pub fn fit(entry: &Entry, machine: &Machine, photos: bool) -> Fit {
    let choice = pick(machine, entry);
    let note = choice
        .note
        .or((machine.on_battery || machine.low_power).then_some(ON_BATTERY));
    let unsupported = |why: &str| Fit {
        verdict: Verdict::NotSupported(why.into()),
        choice,
        smaller: false,
        context: CONTEXT,
        need: 0,
        note,
    };
    if catalog::runtime(&machine.os, &machine.arch, choice.backend).is_none() {
        return unsupported("Letronna has no llama.cpp build for this kind of computer yet.");
    }
    let (gpu, cpu) = room(machine, &choice);
    // On the processor alone, its memory is the fast one.
    let fast = if choice.backend == Backend::Cpu {
        cpu
    } else {
        gpu
    };
    let smaller = entry.smaller.is_some()
        && need(entry, &entry.file, CONTEXT, photos) > fast
        && need(entry, entry.pack(true), CONTEXT, photos) <= fast;
    let pack = entry.pack(smaller);
    let context = context(entry, pack, fast, photos);
    let need = need(entry, pack, context, photos);
    if machine.arch == "x86_64" && !machine.avx2 && (choice.backend == Backend::Cpu || need > gpu) {
        return unsupported(
            "This processor lacks AVX2, so a model would write under two words a second.",
        );
    }
    let (lo, hi) = tokens_per_second(entry, pack, machine, &choice, need, gpu);
    let verdict = if need <= fast && lo >= WELL {
        Verdict::RunsWell { lo, hi }
    } else if need <= gpu + cpu {
        Verdict::Slower { lo, hi }
    } else {
        Verdict::NotEnoughMemory {
            need: need as u64,
            total: machine.ram_total,
            usable: (gpu + cpu) as u64,
        }
    };
    Fit {
        verdict,
        choice,
        smaller,
        context,
        need: need as u64,
        note,
    }
}

/// Bytes `entry` takes running `pack` with `context` tokens.
fn need(entry: &Entry, pack: &Pack, context: u64, photos: bool) -> f64 {
    let file = pack.size as f64;
    let overhead = (0.5 + 0.04 * file / GB).clamp(0.5, 1.0) * GB;
    let kv = (entry.kv_per_token * context) as f64;
    let eyes = match (photos, &entry.eyes) {
        (true, Some(eyes)) => eyes.size as f64 + PHOTOS_MARGIN,
        _ => 0.0,
    };
    file + kv + entry.fixed_state as f64 + overhead + eyes
}

/// The context to start `entry` with: an agent's where it fits, the usual
/// one, or less, but never under `LEAST_CONTEXT`.
fn context(entry: &Entry, pack: &Pack, fast: f64, photos: bool) -> u64 {
    let want = if entry.tools == Tools::Agent {
        AGENT_CONTEXT
    } else {
        CONTEXT
    };
    [want, CONTEXT, LEAST_CONTEXT]
        .into_iter()
        .map(|c| c.min(entry.native_context))
        .find(|&c| need(entry, pack, c, photos) <= fast)
        .unwrap_or(CONTEXT.min(entry.native_context))
}

/// Memory a model may take on the graphics chip and in the system's memory.
fn room(machine: &Machine, choice: &Choice) -> (f64, f64) {
    let system = machine.ram_total as f64 * SYSTEM_SHARE;
    let Some(gpu) = choice.gpu.and_then(|i| machine.gpus.get(i)) else {
        return (0.0, system);
    };
    match gpu.kind {
        Kind::Unified => ((gpu.total as f64).min(system), 0.0),
        Kind::Integrated => (system, 0.0),
        Kind::Discrete => ((gpu.free as f64 - CARD_RESERVE).max(0.0), system),
    }
}

/// The estimated range of tokens a second.
fn tokens_per_second(
    entry: &Entry,
    pack: &Pack,
    machine: &Machine,
    choice: &Choice,
    need: f64,
    gpu_room: f64,
) -> (f64, f64) {
    let active = pack.size as f64 * entry.active;
    let system = system_bandwidth(&machine.cpu) * GB;
    let gpu = choice.gpu.and_then(|i| machine.gpus.get(i));
    // The share of the weights on the graphics chip, the rest spilling to
    // the system's memory.
    let (on_gpu, bandwidth) = match gpu {
        None => (0.0, system),
        Some(g) if g.kind == Kind::Unified => (1.0, apple_bandwidth(&g.name) * GB),
        Some(g) if g.kind == Kind::Integrated => (1.0, system),
        Some(g) => (
            (gpu_room / need).min(1.0),
            card_bandwidth(g.name.as_str(), g.vendor, g.total) * GB,
        ),
    };
    let rate = |e: f64| {
        let seconds = active * on_gpu / (e * bandwidth) + active * (1.0 - on_gpu) / (e * system);
        1.0 / seconds
    };
    (rate(EFFICIENCY.0), rate(EFFICIENCY.1))
}

/// Memory bandwidth of an Apple chip, in GB/s, by its name.
fn apple_bandwidth(name: &str) -> f64 {
    const CHIPS: &[(&str, f64)] = &[
        ("M1", 68.0),
        ("M1 Pro", 200.0),
        ("M1 Max", 400.0),
        ("M1 Ultra", 800.0),
        ("M2", 100.0),
        ("M2 Pro", 200.0),
        ("M2 Max", 400.0),
        ("M2 Ultra", 800.0),
        ("M3", 100.0),
        ("M3 Pro", 150.0),
        ("M3 Max", 300.0),
        ("M3 Ultra", 819.0),
        ("M4", 120.0),
        ("M4 Pro", 273.0),
        ("M4 Max", 410.0),
        ("M5", 153.0),
    ];
    let chip = name.trim().trim_start_matches("Apple").trim();
    if let Some((_, bandwidth)) = CHIPS.iter().find(|(known, _)| *known == chip) {
        return *bandwidth;
    }
    // A chip newer than this table, by its tier.
    if chip.ends_with("Ultra") {
        800.0
    } else if chip.ends_with("Max") {
        410.0
    } else if chip.ends_with("Pro") {
        273.0
    } else {
        120.0
    }
}

/// Memory bandwidth of a graphics card, in GB/s: by name for the common
/// ones, else by how much memory it has.
fn card_bandwidth(name: &str, vendor: Vendor, memory: u64) -> f64 {
    const CARDS: &[(&str, f64)] = &[
        ("RTX 5090", 1792.0),
        ("RTX 5080", 960.0),
        ("RTX 5070 Ti", 896.0),
        ("RTX 5070", 672.0),
        ("RTX 5060 Ti", 448.0),
        ("RTX 5060", 448.0),
        ("RTX 4090", 1008.0),
        ("RTX 4080 SUPER", 736.0),
        ("RTX 4080", 717.0),
        ("RTX 4070 Ti SUPER", 672.0),
        ("RTX 4070 Ti", 504.0),
        ("RTX 4070 SUPER", 504.0),
        ("RTX 4070", 504.0),
        ("RTX 4060 Ti", 288.0),
        ("RTX 4060", 272.0),
        ("RTX 3090 Ti", 1008.0),
        ("RTX 3090", 936.0),
        ("RTX 3080 Ti", 912.0),
        ("RTX 3080", 760.0),
        ("RTX 3070", 448.0),
        ("RTX 3060", 360.0),
        ("RX 7900 XTX", 960.0),
        ("RX 7900 XT", 800.0),
        ("RX 7800 XT", 624.0),
        ("RX 7700 XT", 432.0),
        ("RX 7600", 288.0),
        ("RX 9070 XT", 640.0),
        ("RX 9070", 640.0),
        ("RX 9060 XT", 320.0),
        ("A770", 560.0),
        ("A750", 512.0),
        ("B580", 456.0),
        ("B570", 380.0),
    ];
    let upper = name.to_ascii_uppercase();
    if let Some((_, bandwidth)) = CARDS
        .iter()
        .find(|(card, _)| upper.contains(&card.to_ascii_uppercase()))
    {
        return *bandwidth;
    }
    let gb = memory as f64 / GIB;
    let class = if gb >= 23.0 {
        900.0
    } else if gb >= 15.0 {
        600.0
    } else if gb >= 11.0 {
        450.0
    } else if gb >= 7.0 {
        280.0
    } else {
        200.0
    };
    if vendor == Vendor::Other {
        class * 0.6
    } else {
        class
    }
}

/// Bandwidth of the system's memory in GB/s, dual-channel: DDR5 where the
/// processor only takes DDR5 or LPDDR5, else DDR4's.
fn system_bandwidth(cpu: &str) -> f64 {
    const DDR4: f64 = 40.0;
    const DDR5: f64 = 70.0;
    let cpu = cpu
        .to_ascii_lowercase()
        .replace("(tm)", "")
        .replace("(r)", "");
    if cpu.contains("core ultra") || cpu.contains("ryzen ai") || cpu.contains("snapdragon") {
        return DDR5;
    }
    // Ryzen 7000 and later are DDR5 on the desktop and LPDDR5 in laptops.
    if cpu.contains("ryzen") {
        let number = cpu
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter_map(|word| {
                let digits: String = word.chars().take_while(char::is_ascii_digit).collect();
                digits.parse::<u32>().ok()
            })
            .find(|n| (1000..10_000).contains(n));
        if let Some(number) = number {
            return if number >= 7000 { DDR5 } else { DDR4 };
        }
    }
    DDR4
}

/// The model to suggest here: the best that runs well at a brisk pace, else
/// the best that runs well at all.
pub fn suggest(machine: &Machine) -> Option<&'static Entry> {
    let well: Vec<(&'static Entry, f64)> = CATALOG
        .iter()
        .filter_map(|entry| match fit(entry, machine, false).verdict {
            Verdict::RunsWell { lo, .. } => Some((entry, lo)),
            _ => None,
        })
        .collect();
    let best = |brisk: bool| {
        well.iter()
            .filter(|(_, lo)| !brisk || *lo >= BRISK)
            .max_by_key(|(entry, _)| entry.rank)
            .map(|(entry, _)| *entry)
    };
    best(true).or_else(|| best(false))
}

/// Whether `entry` can run on `machine` at all.
pub fn fits(entry: &Entry, machine: &Machine) -> bool {
    fit(entry, machine, false).verdict.runs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::on_device::machine::tests::{apple, bare, card, pc};

    const GB: u64 = 1_000_000_000;
    const GIB: u64 = 1 << 30;

    fn entry(id: &str) -> &'static Entry {
        catalog::entry(id).unwrap()
    }

    fn label(id: &str, machine: &Machine) -> String {
        fit(entry(id), machine, false).verdict.label()
    }

    fn m1_8gb() -> Machine {
        apple("Apple M1", 8 * GIB, 5_461 << 20)
    }

    fn m5_16gb() -> Machine {
        apple("Apple M5", 16 * GIB, 12_124 << 20)
    }

    /// A card with what Windows' desktop leaves free of it.
    fn nvidia(name: &str, memory: u64, free: u64) -> Machine {
        let mut card = card(name, Vendor::Nvidia, Kind::Discrete, memory, Some("580.88"));
        card.free = free;
        pc("windows", 0, vec![card])
    }

    fn rtx_4090() -> Machine {
        Machine {
            ram_total: 64 * GIB,
            ..nvidia("NVIDIA GeForce RTX 4090", 24 * GIB, 23 * GB)
        }
    }

    fn rtx_4060() -> Machine {
        Machine {
            ram_total: 16 * GIB,
            ..nvidia("NVIDIA GeForce RTX 4060", 8 * GIB, 7_500_000_000)
        }
    }

    fn integrated_32gb() -> Machine {
        Machine {
            cpu: "Intel(R) Core(TM) Ultra 7 155H".into(),
            ..pc(
                "windows",
                32 * GIB,
                vec![card(
                    "Intel(R) Arc(TM) Graphics",
                    Vendor::Intel,
                    Kind::Integrated,
                    2 * GIB,
                    None,
                )],
            )
        }
    }

    fn ddr4_without_avx2() -> Machine {
        Machine {
            cpu: "Intel(R) Core(TM) i5-3570 CPU @ 3.40GHz".into(),
            avx2: false,
            ..bare("linux", "x86_64", 16 * GIB)
        }
    }

    #[test]
    fn an_8gb_m1_runs_the_small_models_and_says_why_not_the_rest() {
        let m1 = m1_8gb();
        assert!(label("bonsai-2-4b", &m1).starts_with("Runs well"));
        assert!(label("bonsai-2-9b", &m1).starts_with("Runs well"));
        assert!(label("gemma-4-e4b", &m1).starts_with("Runs, slower"));
        let qwen = fit(entry("qwen-3.8-27b"), &m1, false).verdict;
        assert_eq!(qwen.label(), "Not enough memory");
        assert_eq!(
            qwen.why().as_deref(),
            Some("Needs 18 GB · this computer has 8 GB (5 GB usable).")
        );
        assert_eq!(suggest(&m1).map(|e| e.id), Some("bonsai-2-4b"));
    }

    #[test]
    fn a_16gb_m5_is_suggested_bonsai_9b() {
        let m5 = m5_16gb();
        assert!(label("bonsai-2-4b", &m5).starts_with("Runs well"));
        assert!(label("qwen-3.5-9b", &m5).starts_with("Runs well"));
        assert!(label("bonsai-2-27b", &m5).starts_with("Runs, slower"));
        assert_eq!(label("qwen-3.8-27b", &m5), "Not enough memory");
        assert_eq!(label("qwen3-coder-next", &m5), "Not enough memory");
        assert_eq!(suggest(&m5).map(|e| e.id), Some("bonsai-2-9b"));
    }

    #[test]
    fn the_speed_estimate_holds_what_the_m5_measured() {
        // Bonsai 2 4B wrote 55 tokens a second under llama-server on Metal.
        let Verdict::RunsWell { lo, hi } = fit(entry("bonsai-2-4b"), &m5_16gb(), false).verdict
        else {
            panic!("Bonsai 2 4B should run well on an M5");
        };
        assert!(lo < 55.0 && 55.0 < hi, "{lo}–{hi}");
        assert_eq!(speed(lo, hi), "44–76 tok/s");
    }

    #[test]
    fn a_4090_with_64gb_is_suggested_qwen_27b_and_takes_the_smaller_35b() {
        let pc = rtx_4090();
        assert_eq!(suggest(&pc).map(|e| e.id), Some("qwen-3.8-27b"));
        let moe = fit(entry("qwen-3.6-35b-a3b"), &pc, false);
        assert!(moe.smaller, "{moe:?}");
        assert!(moe.verdict.label().starts_with("Runs well"));
        assert!(label("qwen3-coder-next", &pc).starts_with("Runs, slower"));
    }

    #[test]
    fn an_8gb_4060_spills_the_big_models_and_is_suggested_bonsai_9b() {
        let pc = rtx_4060();
        assert!(label("bonsai-2-9b", &pc).starts_with("Runs well"));
        assert!(label("qwen-3.5-9b", &pc).starts_with("Runs, slower"));
        assert!(label("bonsai-2-27b", &pc).starts_with("Runs, slower"));
        assert_eq!(label("qwen3-coder-next", &pc), "Not enough memory");
        assert_eq!(suggest(&pc).map(|e| e.id), Some("bonsai-2-9b"));
        assert_eq!(
            fit(entry("bonsai-2-9b"), &pc, false).choice.backend,
            Backend::Cuda13
        );
    }

    #[test]
    fn integrated_graphics_with_32gb_run_the_small_models_from_the_shared_memory() {
        let laptop = integrated_32gb();
        let bonsai = fit(entry("bonsai-2-4b"), &laptop, false);
        assert_eq!(bonsai.choice.backend, Backend::Vulkan);
        assert!(bonsai.verdict.label().starts_with("Runs well"));
        assert!(label("qwen-3.8-27b", &laptop).starts_with("Runs, slower"));
        assert_eq!(suggest(&laptop).map(|e| e.id), Some("bonsai-2-4b"));
    }

    #[test]
    fn a_processor_without_avx2_runs_nothing() {
        let old = ddr4_without_avx2();
        for entry in CATALOG {
            let fit = fit(entry, &old, false);
            assert_eq!(fit.verdict.label(), "Not supported", "{}", entry.id);
            assert!(fit.verdict.why().unwrap().contains("AVX2"));
        }
        assert_eq!(suggest(&old), None);
    }

    #[test]
    fn a_processor_alone_with_ddr4_is_still_suggested_the_smallest() {
        let pc = Machine {
            cpu: "Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz".into(),
            ..bare("linux", "x86_64", 16 * GIB)
        };
        assert!(label("bonsai-2-4b", &pc).starts_with("Runs well"));
        assert!(label("bonsai-2-9b", &pc).starts_with("Runs, slower"));
        assert_eq!(suggest(&pc).map(|e| e.id), Some("bonsai-2-4b"));
    }

    #[test]
    fn photos_cost_the_vision_pack_and_a_margin() {
        let m5 = m5_16gb();
        let bonsai = entry("bonsai-2-4b");
        let without = fit(bonsai, &m5, false).need;
        let with = fit(bonsai, &m5, true).need;
        assert_eq!(with - without, bonsai.eyes.unwrap().size + 300_000_000);
    }

    #[test]
    fn an_agent_gets_the_longer_context_only_where_it_fits() {
        let agent = Entry {
            tools: Tools::Agent,
            ..*entry("bonsai-2-4b")
        };
        assert_eq!(fit(&agent, &m5_16gb(), false).context, AGENT_CONTEXT);
        assert_eq!(
            fit(entry("bonsai-2-4b"), &m5_16gb(), false).context,
            CONTEXT
        );
        let tight = Entry {
            tools: Tools::Agent,
            ..*entry("bonsai-2-27b")
        };
        // Room for its 12k context and not its 16k.
        let m1 = apple("Apple M1", 16 * GIB, 9_060 << 20);
        assert_eq!(fit(&tight, &m1, false).context, LEAST_CONTEXT);
    }

    #[test]
    fn a_kind_of_computer_with_no_build_is_not_supported() {
        let intel_mac = Machine {
            gpus: vec![card(
                "AMD Radeon Pro 5500M",
                Vendor::Amd,
                Kind::Discrete,
                8 * GIB,
                None,
            )],
            ..bare("macos", "x86_64", 16 * GIB)
        };
        let verdict = fit(entry("bonsai-2-4b"), &intel_mac, false).verdict;
        assert_eq!(verdict.label(), "Not supported");
    }

    #[test]
    fn being_on_battery_is_said_beside_the_verdict() {
        let mut m5 = m5_16gb();
        assert_eq!(fit(entry("bonsai-2-4b"), &m5, false).note, None);
        m5.on_battery = true;
        assert_eq!(fit(entry("bonsai-2-4b"), &m5, false).note, Some(ON_BATTERY));
    }

    #[test]
    fn apple_chips_are_told_apart_by_their_tier() {
        assert_eq!(apple_bandwidth("Apple M1"), 68.0);
        assert_eq!(apple_bandwidth("Apple M1 Pro"), 200.0);
        assert_eq!(apple_bandwidth("Apple M4 Max"), 410.0);
        assert_eq!(apple_bandwidth("Apple M5"), 153.0);
        assert_eq!(apple_bandwidth("Apple M5 Pro"), 273.0);
        assert_eq!(apple_bandwidth("Apple M9"), 120.0);
    }

    #[test]
    fn system_memory_speed_follows_the_processor() {
        assert_eq!(
            system_bandwidth("AMD Ryzen 9 7950X 16-Core Processor"),
            70.0
        );
        assert_eq!(
            system_bandwidth("AMD Ryzen 7 5800X3D 8-Core Processor"),
            40.0
        );
        assert_eq!(system_bandwidth("Intel(R) Core(TM) Ultra 7 155H"), 70.0);
        assert_eq!(
            system_bandwidth("Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz"),
            40.0
        );
    }

    #[test]
    fn sizes_and_memory_read_as_people_count_them() {
        assert_eq!(size(1_212_103_520), "1.2 GB");
        assert_eq!(size(366_894_624), "367 MB");
        assert_eq!(size(38_429_272_064), "38 GB");
        assert_eq!(memory(16 * GIB), "16 GB");
        assert_eq!(speed(6.2, 9.4), "6–9 tok/s");
    }
}
