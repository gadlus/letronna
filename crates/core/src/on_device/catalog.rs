//! The models Letronna offers to download, and the llama.cpp builds that run
//! them.
//!
//! Every file is named by its exact size and SHA-256 and fetched from a
//! fixed commit, not a branch: the repositories re-convert and have renamed
//! files before, and a file that moved must fail as moved rather than
//! download something else. All eight are Apache-2.0. Left out on purpose:
//! gpt-oss-20b (Qwen3.5 9B beats it at half the size, and its format cannot
//! turn thinking off), LFM2.5-24B-A2B (its licence stops at $10M revenue),
//! and Bonsai's PQ2_0, PTQ1_0, MTP and DFlash2 files, which only PrismML's
//! own fork of llama.cpp runs.
//!
//! Memory figures here are estimates, from the files' layer counts: Qwen3.5,
//! Qwen3.8 and Bonsai are hybrid models where one layer in four keeps a
//! KV cache, so a long context costs them little.

use super::machine::Backend;

/// A file a model needs, pinned to one commit of its repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pack {
    pub repo: &'static str,
    pub commit: &'static str,
    pub file: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

impl Pack {
    /// Where it is downloaded from: the resolve address answers with a
    /// redirect to a signed copy that lasts an hour, so it is asked again
    /// for every attempt.
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.commit, self.file
        )
    }
}

/// How a model is asked to pick its words.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sampling {
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: u32,
    pub presence_penalty: f64,
}

/// Qwen's own advice for answering without thinking, which the phone app
/// measured as the best of what it tried.
pub const SAMPLING: Sampling = Sampling {
    temperature: 0.7,
    top_p: 0.8,
    top_k: 20,
    presence_penalty: 1.5,
};

/// Whether a model has a thinking mode to turn off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    /// It thinks unless told not to, which costs a small model most of its
    /// answer; every request turns it off.
    Off,
    /// It never thinks, and its template knows no switch for it.
    Never,
}

/// What a model is trusted with, as measured by the tool-call eval and never
/// guessed: a model that calls a tool badly is worse than one with none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tools {
    /// Chat only.
    Chat,
    /// Chat, and a web search while online.
    Search,
    /// Letronna's tools, kept to those that stay on this computer.
    Agent,
}

/// A model that can be downloaded to run here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    /// What settings and sessions call it; never changes once shipped.
    pub id: &'static str,
    pub name: &'static str,
    pub maker: &'static str,
    pub file: Pack,
    /// A smaller quantization, taken where the main file would not run well.
    pub smaller: Option<Pack>,
    /// Its vision pack, for reading photos; fetched the first time one is sent.
    pub eyes: Option<Pack>,
    /// Bytes of KV cache each token of context holds, at f16.
    pub kv_per_token: u64,
    /// Bytes held whatever the context: the recurrent layers' state.
    pub fixed_state: u64,
    /// Share of the weights read for each token: 1 for a dense model, about
    /// active over total parameters for a mixture of experts.
    pub active: f64,
    /// Answer quality against the others, higher is better.
    pub rank: u8,
    /// The longest context it was trained for.
    pub native_context: u64,
    pub sampling: Sampling,
    pub thinking: Thinking,
    pub tools: Tools,
}

impl Entry {
    /// Ask this model as it answers best: its sampling, its thinking
    /// turned off, llama-server's own reading of any thoughts, and a reply
    /// held to `cap` tokens.
    pub fn shape(&self, body: &mut serde_json::Value, cap: u64) {
        let Some(body) = body.as_object_mut() else {
            return;
        };
        let s = self.sampling;
        body.insert("temperature".into(), s.temperature.into());
        body.insert("top_p".into(), s.top_p.into());
        body.insert("top_k".into(), s.top_k.into());
        body.insert("presence_penalty".into(), s.presence_penalty.into());
        if self.thinking == Thinking::Off {
            let kwargs = body
                .entry("chat_template_kwargs")
                .or_insert_with(|| serde_json::json!({}));
            if let Some(kwargs) = kwargs.as_object_mut() {
                kwargs.insert("enable_thinking".into(), false.into());
            }
        }
        body.insert("reasoning_format".into(), "auto".into());
        let asked = body
            .get("max_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(cap);
        body.insert("max_tokens".into(), asked.min(cap).into());
        body.remove("reasoning_effort");
    }

    /// The file to run on a machine, given whether the main one fits there.
    pub fn pack(&self, smaller: bool) -> &Pack {
        match (smaller, &self.smaller) {
            (true, Some(pack)) => pack,
            _ => &self.file,
        }
    }
}

const KIB: u64 = 1024;
const MB: u64 = 1_000_000;

const BONSAI_4B: &str = "prism-ml/Ternary-Bonsai-2-4B-gguf";
const BONSAI_4B_AT: &str = "8d6557168168f91ea6697a4a49ed2ef622b53adb";
const BONSAI_9B: &str = "prism-ml/Ternary-Bonsai-2-9B-gguf";
const BONSAI_9B_AT: &str = "5b2abb02362efbed46bbe204014b02837778b2c2";
const QWEN_9B: &str = "unsloth/Qwen3.5-9B-GGUF";
const QWEN_9B_AT: &str = "3885219b6810b007914f3a7950a8d1b469d598a5";
const GEMMA: &str = "ggml-org/gemma-4-E4B-it-GGUF";
const GEMMA_AT: &str = "a69f018d7678a2b9f5e705f8903d6fb29b9b25dc";
// The 27B's main repository has only the files that need PrismML's fork;
// its dev repository's Q2_0 runs on llama.cpp since b11556 (#29600),
// whatever its name says. Its vision pack is only in the main one.
const BONSAI_27B: &str = "prism-ml/Ternary-Bonsai-2-27B-gguf-dev";
const BONSAI_27B_AT: &str = "2a263ef827a2e215f3ddd14c9871a5bd1800fcbc";
const BONSAI_27B_EYES: &str = "prism-ml/Ternary-Bonsai-2-27B-gguf";
const BONSAI_27B_EYES_AT: &str = "b072e1d3b35a0a630cece372c2127528e0994386";
const QWEN_27B: &str = "unsloth/Qwen3.8-27B-GGUF";
const QWEN_27B_AT: &str = "4ca720788d1e01f1bff70c033e0d0028fd02e502";
const QWEN_35B: &str = "unsloth/Qwen3.6-35B-A3B-GGUF";
const QWEN_35B_AT: &str = "a483e9e6cbd595906af30beda3187c2663a1118c";
const CODER: &str = "unsloth/Qwen3-Coder-Next-GGUF";
const CODER_AT: &str = "ce09c67b53bc8739eef83fe67b2f5d293c270632";

/// Every model offered, smallest first.
pub static CATALOG: &[Entry] = &[
    Entry {
        id: "bonsai-2-4b",
        name: "Ternary Bonsai 2 · 4B",
        maker: "PrismML",
        file: Pack {
            repo: BONSAI_4B,
            commit: BONSAI_4B_AT,
            file: "Ternary-Bonsai-2-4B-Q2_0.gguf",
            size: 1_212_103_520,
            sha256: "64a5183b44921bdb5ac4a6f5f92007821d6ebe5c66997d3c147fd214ab9433d4",
        },
        smaller: None,
        // Q8_0 rather than BF16: Bonsai's card says BF16 can crash
        // llama-server under images sent at once.
        eyes: Some(Pack {
            repo: BONSAI_4B,
            commit: BONSAI_4B_AT,
            file: "Ternary-Bonsai-2-4B-mmproj-Q8_0.gguf",
            size: 366_894_624,
            sha256: "db939d5f9b7014137810205e91d05e5f54afd72b0b3af9d33bd7d82f7906a55d",
        }),
        kv_per_token: 32 * KIB,
        fixed_state: 50 * MB,
        active: 1.0,
        rank: 1,
        native_context: 262_144,
        sampling: SAMPLING,
        thinking: Thinking::Off,
        // 11/12 then 12/12 on the phone app's search eval.
        tools: Tools::Search,
    },
    Entry {
        id: "gemma-4-e4b",
        name: "Gemma 4 · E4B",
        maker: "Google",
        // The QAT build: trained for 4 bits, so Q4_0 loses little.
        file: Pack {
            repo: GEMMA,
            commit: GEMMA_AT,
            file: "gemma-4-E4B-it-Q4_0.gguf",
            size: 4_590_807_392,
            sha256: "a555b900214b477d8880e7832e0b8925e139b0159640036b09fe472b6f2097f2",
        },
        smaller: None,
        eyes: Some(Pack {
            repo: GEMMA,
            commit: GEMMA_AT,
            file: "mmproj-gemma-4-E4B-it-Q8_0.gguf",
            size: 559_874_816,
            sha256: "197f49a93027f9843772bd24a6a9e0be2a32a788de5a3def330e9c585d86edd1",
        }),
        // Sliding-window layers keep a fixed span; only its global layers
        // grow with the context.
        kv_per_token: 16 * KIB,
        fixed_state: 25 * MB,
        active: 1.0,
        rank: 2,
        native_context: 131_072,
        // Its own, as stored in the GGUF.
        sampling: Sampling {
            temperature: 1.0,
            top_p: 0.95,
            top_k: 64,
            presence_penalty: 0.0,
        },
        thinking: Thinking::Off,
        tools: Tools::Chat,
    },
    Entry {
        id: "bonsai-2-9b",
        name: "Ternary Bonsai 2 · 9B",
        maker: "PrismML",
        file: Pack {
            repo: BONSAI_9B,
            commit: BONSAI_9B_AT,
            file: "Ternary-Bonsai-2-9B-Q2_0.gguf",
            size: 2_556_640_128,
            sha256: "1f884aa56aacd2bbbb2e4b7abaead2b1102b74a9c75a23fc33dab916813a384d",
        },
        smaller: None,
        eyes: Some(Pack {
            repo: BONSAI_9B,
            commit: BONSAI_9B_AT,
            file: "Ternary-Bonsai-2-9B-mmproj-Q8_0.gguf",
            size: 624_229_664,
            sha256: "e0cb6f6988cea323ef798cd77d019bbb819af75582f034eaab0e7232314953db",
        }),
        kv_per_token: 32 * KIB,
        fixed_state: 50 * MB,
        active: 1.0,
        rank: 3,
        native_context: 262_144,
        sampling: SAMPLING,
        thinking: Thinking::Off,
        // 12/12 twice on the search eval, asked as Letronna asks it.
        tools: Tools::Search,
    },
    Entry {
        id: "qwen-3.5-9b",
        name: "Qwen3.5 · 9B",
        maker: "Qwen",
        file: Pack {
            repo: QWEN_9B,
            commit: QWEN_9B_AT,
            file: "Qwen3.5-9B-Q4_K_M.gguf",
            size: 5_680_522_464,
            sha256: "03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8",
        },
        smaller: None,
        eyes: Some(Pack {
            repo: QWEN_9B,
            commit: QWEN_9B_AT,
            file: "mmproj-F16.gguf",
            size: 918_166_080,
            sha256: "f70dc3509053962b0d0d3ee8a7eacebf5d60aa560cad78254ae8698516ae029f",
        }),
        kv_per_token: 32 * KIB,
        fixed_state: 100 * MB,
        active: 1.0,
        rank: 4,
        native_context: 262_144,
        sampling: SAMPLING,
        thinking: Thinking::Off,
        tools: Tools::Chat,
    },
    // Known to write malformed tool calls, to take exactly one system
    // message and only first, and to answer 500 to empty tool arguments,
    // which are sent as "{}"; it stays Chat until the eval says otherwise.
    Entry {
        id: "bonsai-2-27b",
        name: "Ternary Bonsai 2 · 27B",
        maker: "PrismML",
        file: Pack {
            repo: BONSAI_27B,
            commit: BONSAI_27B_AT,
            file: "Ternary-Bonsai-2-27B-Q2_0-prism-fork-required.gguf",
            size: 7_626_008_928,
            sha256: "4f99aed01b8a877e153f9aa6569a4440fe17c59701cc0953e36d7f460549e70e",
        },
        smaller: None,
        eyes: Some(Pack {
            repo: BONSAI_27B_EYES,
            commit: BONSAI_27B_EYES_AT,
            file: "Ternary-Bonsai-2-27B-mmproj-Q8_0.gguf",
            size: 629_246_976,
            sha256: "6807ede61d570bb86ba34b756a0fa109edc33668604de867c6ea6d8f1d631903",
        }),
        kv_per_token: 64 * KIB,
        fixed_state: 180 * MB,
        active: 1.0,
        rank: 5,
        native_context: 262_144,
        sampling: SAMPLING,
        thinking: Thinking::Off,
        tools: Tools::Chat,
    },
    Entry {
        id: "qwen-3.6-35b-a3b",
        name: "Qwen3.6 · 35B-A3B",
        maker: "Qwen",
        file: Pack {
            repo: QWEN_35B,
            commit: QWEN_35B_AT,
            file: "Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf",
            size: 22_360_456_160,
            sha256: "707a55a8a4397ecde44de0c499d3e68c1ad1d240d1da65826b4949d1043f4450",
        },
        // For a 32 GB Mac or a 24 GB graphics card.
        smaller: Some(Pack {
            repo: QWEN_35B,
            commit: QWEN_35B_AT,
            file: "Qwen3.6-35B-A3B-UD-IQ4_XS.gguf",
            size: 17_730_509_792,
            sha256: "649d7508507b84638732c4f52c24c8b15843c6dca2f3ff793ae07c14a67ebbb3",
        }),
        eyes: Some(Pack {
            repo: QWEN_35B,
            commit: QWEN_35B_AT,
            file: "mmproj-F16.gguf",
            size: 899_283_680,
            sha256: "8971ee4f331ff0a4c609374f32984b3d4e6dc086c0aa35f1d637fad1829e887f",
        }),
        kv_per_token: 20 * KIB,
        fixed_state: 20 * MB,
        // 3B of 35B parameters, plus the attention and embeddings every
        // token reads.
        active: 0.1,
        rank: 6,
        native_context: 262_144,
        sampling: SAMPLING,
        thinking: Thinking::Off,
        tools: Tools::Chat,
    },
    Entry {
        id: "qwen-3.8-27b",
        name: "Qwen3.8 · 27B",
        maker: "Qwen",
        file: Pack {
            repo: QWEN_27B,
            commit: QWEN_27B_AT,
            file: "Qwen3.8-27B-UD-Q4_K_XL.gguf",
            size: 17_559_178_144,
            sha256: "3f227079003add2511437e5b1e94812e363385225bf6a9b47b0054a72bc8b01e",
        },
        smaller: None,
        eyes: Some(Pack {
            repo: QWEN_27B,
            commit: QWEN_27B_AT,
            file: "mmproj-F16.gguf",
            size: 927_607_488,
            sha256: "cbb841a9ee0636b2ec172f5bb8df2ea8dfeb01e90fe7c6126581d662a0b4e43e",
        }),
        kv_per_token: 64 * KIB,
        fixed_state: 150 * MB,
        active: 1.0,
        rank: 7,
        native_context: 262_144,
        sampling: SAMPLING,
        thinking: Thinking::Off,
        tools: Tools::Chat,
    },
    Entry {
        id: "qwen3-coder-next",
        name: "Qwen3-Coder-Next",
        maker: "Qwen",
        file: Pack {
            repo: CODER,
            commit: CODER_AT,
            file: "Qwen3-Coder-Next-UD-IQ4_XS.gguf",
            size: 38_429_272_064,
            sha256: "abf56d7fe8a0a99c15d220c13de4aa57b69cfba6ef4c2a007b56e34d7b40cd11",
        },
        smaller: None,
        eyes: None,
        kv_per_token: 24 * KIB,
        fixed_state: 120 * MB,
        // 3B of 80B parameters.
        active: 0.045,
        rank: 8,
        native_context: 262_144,
        sampling: Sampling {
            temperature: 1.0,
            top_p: 0.95,
            top_k: 40,
            presence_penalty: 0.0,
        },
        thinking: Thinking::Never,
        tools: Tools::Chat,
    },
];

/// The model with this id, if Letronna offers it.
pub fn entry(id: &str) -> Option<&'static Entry> {
    CATALOG.iter().find(|entry| entry.id == id)
}

/// The llama.cpp build every runtime below is: b11556 is the first that runs
/// Bonsai 2's Q2_0, b11772 the first with Gemma 4's vision fix.
pub const BUILD: u32 = 11809;

/// A file of a llama.cpp release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

impl Asset {
    /// Where the release keeps it.
    pub fn url(&self) -> String {
        format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/b{BUILD}/{}",
            self.name
        )
    }
}

/// A llama.cpp build for one system and backend, with the CUDA runtime it
/// leans on where that comes separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Runtime {
    pub os: &'static str,
    pub arch: &'static str,
    pub backend: Backend,
    pub archive: Asset,
    pub extra: Option<Asset>,
}

// The release has no checksum file; these are the digests GitHub's API
// gives each asset, which `gh attestation verify` also passes. ROCm is left
// out: on RDNA3, Vulkan is faster.
pub static RUNTIMES: &[Runtime] = &[
    Runtime {
        os: "macos",
        arch: "aarch64",
        backend: Backend::Metal,
        archive: Asset {
            name: "llama-b11809-bin-macos-arm64.tar.gz",
            size: 16_145_928,
            sha256: "8787642ae0b84a06daaf86321b9007b8986e6089eb4077e54c6af762d9313055",
        },
        extra: None,
    },
    Runtime {
        os: "windows",
        arch: "x86_64",
        backend: Backend::Vulkan,
        archive: Asset {
            name: "llama-b11809-bin-win-vulkan-x64.zip",
            size: 36_868_145,
            sha256: "4722c7cc380007d58b7d3ea427ecd25e70d287f43878a427168af1339d10ca69",
        },
        extra: None,
    },
    Runtime {
        os: "windows",
        arch: "x86_64",
        backend: Backend::Cpu,
        archive: Asset {
            name: "llama-b11809-bin-win-cpu-x64.zip",
            size: 24_070_455,
            sha256: "eabec16af2b221639d865d17f7e3ed654e53dfe5f26ff7823b1f7010e6c90a1f",
        },
        extra: None,
    },
    Runtime {
        os: "windows",
        arch: "x86_64",
        backend: Backend::Cuda13,
        archive: Asset {
            name: "llama-b11809-bin-win-cuda-13.4-x64.zip",
            size: 159_842_150,
            sha256: "521bf9bd321221324d91d824beafa532545f87604cbbab98383a22c9464a9f6f",
        },
        extra: Some(Asset {
            name: "cudart-llama-bin-win-cuda-13.4-x64.zip",
            size: 423_535_356,
            sha256: "738f8c251ac22b70c3ae6f83a10cf222725df0395246a2cf58f32bdb85fbe668",
        }),
    },
    Runtime {
        os: "windows",
        arch: "x86_64",
        backend: Backend::Cuda12,
        archive: Asset {
            name: "llama-b11809-bin-win-cuda-12.4-x64.zip",
            size: 272_317_771,
            sha256: "a07da90730ac78983e61e56302313db108a66eea1bbf55af35861f73476ac88b",
        },
        extra: Some(Asset {
            name: "cudart-llama-bin-win-cuda-12.4-x64.zip",
            size: 391_443_627,
            sha256: "8c79a9b226de4b3cacfd1f83d24f962d0773be79f1e7b75c6af4ded7e32ae1d6",
        }),
    },
    // Windows on ARM runs the x64 app, which downloads the arm64 runtime.
    Runtime {
        os: "windows",
        arch: "aarch64",
        backend: Backend::Vulkan,
        archive: Asset {
            name: "llama-b11809-bin-win-vulkan-arm64.zip",
            size: 25_080_073,
            sha256: "ef020ed26b1526964878952a101485ee51a724204417e360a1da9e07160e4b9b",
        },
        extra: None,
    },
    Runtime {
        os: "windows",
        arch: "aarch64",
        backend: Backend::Cpu,
        archive: Asset {
            name: "llama-b11809-bin-win-cpu-arm64.zip",
            size: 12_693_908,
            sha256: "8f7012e12d68c29aac41b80787af1125a00fca3124cfcab70005645b659e64b8",
        },
        extra: None,
    },
    Runtime {
        os: "linux",
        arch: "x86_64",
        backend: Backend::Vulkan,
        archive: Asset {
            name: "llama-b11809-bin-ubuntu-vulkan-x64.tar.gz",
            size: 34_975_177,
            sha256: "155e79a1a42cccc13906b8f37039d7d502f003e1f789e46329d803414bda261c",
        },
        extra: None,
    },
    Runtime {
        os: "linux",
        arch: "x86_64",
        backend: Backend::Cpu,
        archive: Asset {
            name: "llama-b11809-bin-ubuntu-x64.tar.gz",
            size: 22_332_314,
            sha256: "f0011ccd546d4a223189634e74dd3e3a465ec6b6c07cc4c9a999231560dee03e",
        },
        extra: None,
    },
    Runtime {
        os: "linux",
        arch: "x86_64",
        backend: Backend::Cuda13,
        archive: Asset {
            name: "llama-b11809-bin-ubuntu-cuda-13.4-x64.tar.gz",
            size: 158_791_082,
            sha256: "9d4ae8ce3c752330eeb386d229bf18c1713942a31dcaa521969cabd2b7f42e67",
        },
        extra: Some(Asset {
            name: "cudart-llama-b11809-bin-ubuntu-cuda-13.4-x64.tar.gz",
            size: 440_236_662,
            sha256: "0aef28b0311369aeb46517f6e5453bd02fbdfab1852c5e55331e3175613bb401",
        }),
    },
    // Linux's older CUDA build is 12.8, not Windows' 12.4; both run on a
    // driver from 525 on.
    Runtime {
        os: "linux",
        arch: "x86_64",
        backend: Backend::Cuda12,
        archive: Asset {
            name: "llama-b11809-bin-ubuntu-cuda-12.8-x64.tar.gz",
            size: 177_926_921,
            sha256: "e799c1e68ab1d3538461535af3503727450bd15ea6a8329acbe9b226a73712b0",
        },
        extra: Some(Asset {
            name: "cudart-llama-b11809-bin-ubuntu-cuda-12.8-x64.tar.gz",
            size: 594_378_581,
            sha256: "9594075b511ce54b3c3a78dbe1ea937a4798461ef1bb7934904d3dcabe2414a0",
        }),
    },
    Runtime {
        os: "linux",
        arch: "aarch64",
        backend: Backend::Vulkan,
        archive: Asset {
            name: "llama-b11809-bin-ubuntu-vulkan-arm64.tar.gz",
            size: 28_328_977,
            sha256: "3995f4d2516a87bca49fe03d6f57bbf736e7c02e7bd068f2ddf4071313eb8b84",
        },
        extra: None,
    },
    Runtime {
        os: "linux",
        arch: "aarch64",
        backend: Backend::Cpu,
        archive: Asset {
            name: "llama-b11809-bin-ubuntu-arm64.tar.gz",
            size: 18_038_275,
            sha256: "d88445ab1dc57b41fdb44b4f33f5af6f08fc25d43b235099c520caa9e352dc7f",
        },
        extra: None,
    },
];

/// The runtime for a system, an architecture and a backend, if there is one.
pub fn runtime(os: &str, arch: &str, backend: Backend) -> Option<&'static Runtime> {
    RUNTIMES
        .iter()
        .find(|r| r.os == os && r.arch == arch && r.backend == backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_hex(text: &str, len: usize) -> bool {
        text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit())
    }

    fn packs() -> impl Iterator<Item = &'static Pack> {
        CATALOG.iter().flat_map(|entry| {
            [
                Some(&entry.file),
                entry.smaller.as_ref(),
                entry.eyes.as_ref(),
            ]
            .into_iter()
            .flatten()
        })
    }

    #[test]
    fn every_model_has_an_id_of_its_own() {
        for (i, entry) in CATALOG.iter().enumerate() {
            assert!(
                CATALOG[..i].iter().all(|other| other.id != entry.id),
                "{} twice",
                entry.id
            );
            assert!(
                entry.id.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'-'
                    || b == b'.')
            );
            assert_eq!(super::entry(entry.id), Some(entry));
        }
    }

    #[test]
    fn every_file_is_pinned_to_a_commit_a_size_and_a_hash() {
        for pack in packs() {
            assert!(is_hex(pack.commit, 40), "{}", pack.file);
            assert!(is_hex(pack.sha256, 64), "{}", pack.file);
            assert!(pack.size > 0, "{}", pack.file);
            assert!(pack.file.ends_with(".gguf"), "{}", pack.file);
            assert!(
                pack.url().contains(&format!("/resolve/{}/", pack.commit)),
                "{}",
                pack.url()
            );
        }
    }

    #[test]
    fn the_vision_packs_are_the_small_files_beside_their_models() {
        for entry in CATALOG {
            if let Some(eyes) = entry.eyes {
                assert!(eyes.size < entry.file.size / 2, "{}", entry.id);
                assert!(eyes.file.contains("mmproj"), "{}", entry.id);
            }
        }
        assert!(entry("qwen3-coder-next").is_some_and(|e| e.eyes.is_none()));
        assert!(CATALOG.iter().filter(|e| e.eyes.is_some()).count() == 7);
    }

    #[test]
    fn the_ranks_order_every_model_once() {
        let mut ranks: Vec<u8> = CATALOG.iter().map(|e| e.rank).collect();
        ranks.sort_unstable();
        ranks.dedup();
        assert_eq!(ranks.len(), CATALOG.len());
    }

    #[test]
    fn only_a_model_the_eval_passed_searches() {
        let searching: Vec<&str> = CATALOG
            .iter()
            .filter(|e| e.tools != Tools::Chat)
            .map(|e| e.id)
            .collect();
        assert_eq!(searching, ["bonsai-2-4b", "bonsai-2-9b"]);
    }

    #[test]
    fn a_request_carries_the_models_sampling_and_turns_its_thinking_off() {
        let mut body = serde_json::json!({"max_tokens": 32_000, "reasoning_effort": "high"});
        entry("bonsai-2-4b").unwrap().shape(&mut body, 2048);
        assert_eq!(
            body,
            serde_json::json!({
                "temperature": 0.7, "top_p": 0.8, "top_k": 20, "presence_penalty": 1.5,
                "chat_template_kwargs": {"enable_thinking": false},
                "reasoning_format": "auto", "max_tokens": 2048,
            })
        );
        let mut body = serde_json::json!({"max_tokens": 100});
        entry("qwen3-coder-next").unwrap().shape(&mut body, 2048);
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["top_k"], 40);
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn every_platform_the_window_ships_on_has_a_runtime_and_a_fallback() {
        for (os, arch) in [
            ("macos", "aarch64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
            ("linux", "x86_64"),
        ] {
            let found: Vec<Backend> = RUNTIMES
                .iter()
                .filter(|r| r.os == os && r.arch == arch)
                .map(|r| r.backend)
                .collect();
            assert!(!found.is_empty(), "{os} {arch}");
            if os != "macos" {
                assert!(found.contains(&Backend::Cpu), "{os} {arch}");
            }
        }
    }

    #[test]
    fn every_runtime_is_this_build_with_a_size_and_a_hash() {
        for r in RUNTIMES {
            for asset in [Some(r.archive), r.extra].into_iter().flatten() {
                assert!(is_hex(asset.sha256, 64), "{}", asset.name);
                assert!(asset.size > 1_000_000, "{}", asset.name);
                assert!(asset.url().contains(&format!("/b{BUILD}/")));
            }
            assert!(r.archive.name.contains(&format!("b{BUILD}")));
            assert_eq!(r.extra.is_some(), r.backend.is_cuda(), "{}", r.archive.name);
        }
    }
}
