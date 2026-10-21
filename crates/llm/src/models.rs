//! The language model catalogue.
//!
//! Every candidate is a small instruct model released under Apache-2.0, quantized to GGUF by its
//! own authors (Qwen, Hugging Face). Each file is pinned to a Hugging Face commit and checked
//! against its SHA-256 and size when it is downloaded (into `<data dir>/models/<id>/`, after the
//! user opts in). Weights are never bundled with FilmCraft or committed to the repository.
//!
//! The tokenizer comes from each family's base (non-GGUF) repository; the 0.5B/1.5B Qwen2.5 and
//! the 360M/1.7B SmolLM2 repositories carry byte-identical `tokenizer.json` files (same git blob),
//! so one checksum per family.

use std::path::{Path, PathBuf};

pub use filmcraft_speech::models::ModelFile;
use serde::Serialize;

/// Model family: decides the GGUF architecture and the chat template.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Family {
    /// Qwen2.5 Instruct (GGUF architecture `qwen2`), ChatML template.
    Qwen25,
    /// SmolLM2 Instruct (GGUF architecture `llama`), ChatML template.
    SmolLm2,
    /// SmolLM3 (GGUF architecture `smollm3`: Llama-style, no rotary embedding on every 4th layer),
    /// its ChatML-style template with a reasoning-mode system header (used in `/no_think` mode).
    SmolLm3,
}

impl Family {
    /// The `general.architecture` value in the GGUF header.
    pub fn architecture(self) -> &'static str {
        match self {
            Family::Qwen25 => "qwen2",
            Family::SmolLm2 => "llama",
            Family::SmolLm3 => "smollm3",
        }
    }

    /// The token that ends an assistant turn.
    pub fn end_of_turn(self) -> &'static str {
        "<|im_end|>"
    }

    /// The tokenizer's end-of-text token (also a stop token).
    pub fn end_of_text(self) -> &'static str {
        match self {
            Family::Qwen25 | Family::SmolLm2 => "<|endoftext|>",
            // the Llama 3 vocabulary
            Family::SmolLm3 => "<|end_of_text|>",
        }
    }
}

/// A downloadable language model.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub family: Family,
    pub description: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub author: &'static str,
    /// Human-readable source page.
    pub source: &'static str,
    /// The GGUF weights (one of `files`).
    pub weights: &'static str,
    /// The `tokenizer.json` (one of `files`).
    pub tokenizer: &'static str,
    pub files: &'static [ModelFile],
}

impl ModelInfo {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).fold(0u64, u64::saturating_add)
    }

    /// Whether some file has no download URL yet: a locally built model (the fork's fine-tuned
    /// explainers until they are published); its files are put in its folder by hand or by the
    /// training session (docs/fork/TRAINING.md).
    pub fn is_local(&self) -> bool {
        self.files.iter().any(|f| f.url.is_empty())
    }

    /// Whether this is one of the fork's fine-tuned explainers (GRADE-PLAN G2.11b).
    pub fn is_tuned(&self) -> bool {
        self.id.starts_with(TUNED_PREFIX) || self.id.starts_with(ASSIST_TUNED_PREFIX)
    }

    /// The generation settings this model is evaluated with. The tuned explainers write a
    /// controlled grammar that repeats small words ("the", "by", "stop") by design, so they run
    /// without a repeat penalty (with the default 1.1 the tuned SmolLM2-360M passed 58 % of the
    /// held-out cases instead of 94 %); stock models keep the defaults.
    pub fn recommended_options(&self) -> crate::GenOptions {
        let d = crate::GenOptions::default();
        if self.is_tuned() { crate::GenOptions { repeat_penalty: 1.0, ..d } } else { d }
    }

    /// The catalogue entry of file `name`.
    pub fn file(&self, name: &str) -> Option<&'static ModelFile> {
        self.files.iter().find(|f| f.name == name)
    }
}

/// The id prefix of the fork's fine-tuned explainers (GRADE-PLAN G2.11b, docs/fork/TRAINING.md).
pub const TUNED_PREFIX: &str = "filmcraft-explainer-";
/// The id prefix of the fork's fine-tuned assistant models (FORK-PLAN §1a.97).
pub const ASSIST_TUNED_PREFIX: &str = "filmcraft-assistant-";

/// Where the fork publishes its fine-tuned explainers (docs/fork/TRAINING.md, "Publishing"):
/// `<FORK_RELEASES><tag>/<file>`.
pub const FORK_RELEASES: &str = "https://github.com/dawgon2/revolution/releases/download/";

const APACHE: &str = "Apache-2.0";
const APACHE_URL: &str = "https://www.apache.org/licenses/LICENSE-2.0";

macro_rules! hf {
    ($repo:literal, $rev:literal, $name:literal, $sha:literal, $size:expr) => {
        ModelFile { name: $name, url: concat!("https://huggingface.co/", $repo, "/resolve/", $rev, "/", $name), sha256: $sha, size: $size }
    };
}

/// A fine-tune published as a release asset of the fork: `<FORK_RELEASES><tag>/<name>`.
macro_rules! fork_release {
    ($tag:literal, $name:literal, $sha:literal, $size:expr) => {
        ModelFile { name: $name, url: concat!("https://github.com/dawgon2/revolution/releases/download/", $tag, "/", $name), sha256: $sha, size: $size }
    };
}

/// A file with no download URL yet (a locally built model, see [`ModelInfo::is_local`]).
macro_rules! local {
    ($name:literal, $sha:literal, $size:expr) => {
        ModelFile { name: $name, url: "", sha256: $sha, size: $size }
    };
}

const QWEN_TOKENIZER: ModelFile = hf!(
    "Qwen/Qwen2.5-0.5B-Instruct",
    "7ae557604adf67be50417f59c2c2f167def9a775",
    "tokenizer.json",
    "c0382117ea329cdf097041132f6d735924b697924d6f6fc3945713e96ce87539",
    7031645
);
const SMOL_TOKENIZER: ModelFile = hf!(
    "HuggingFaceTB/SmolLM2-360M-Instruct",
    "a10cc1512eabd3dde888204e902eca88bddb4951",
    "tokenizer.json",
    "9ca9acddb6525a194ec8ac7a87f24fbba7232a9a15ffa1af0c1224fcd888e47c",
    2104556
);

const SMOL3_TOKENIZER: ModelFile = hf!(
    "HuggingFaceTB/SmolLM3-3B",
    "a07cc9a04f16550a088caea529712d1d335b0ac1",
    "tokenizer.json",
    "7b6a500b662a34eb3f0374db856ba4ad7de4c81040571d78dc0d357238930005",
    17208819
);

static CATALOGUE: &[ModelInfo] = &[
    ModelInfo {
        id: "qwen2.5-0.5b-instruct-q4km",
        name: "Qwen2.5 0.5B Instruct (4-bit)",
        family: Family::Qwen25,
        description: "0.5 B parameters, 4-bit. The default (provisional until the 30-case evaluation): fast on any CPU.",
        license: APACHE,
        license_url: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/blob/main/LICENSE",
        author: "Qwen team, Alibaba Cloud",
        source: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF",
        weights: "qwen2.5-0.5b-instruct-q4_k_m.gguf",
        tokenizer: "tokenizer.json",
        files: &[
            hf!(
                "Qwen/Qwen2.5-0.5B-Instruct-GGUF",
                "9217f5db79a29953eb74d5343926648285ec7e67",
                "qwen2.5-0.5b-instruct-q4_k_m.gguf",
                "74a4da8c9fdbcd15bd1f6d01d621410d31c6fc00986f5eb687824e7b93d7a9db",
                491400032
            ),
            QWEN_TOKENIZER,
        ],
    },
    ModelInfo {
        id: "qwen2.5-1.5b-instruct-q4km",
        name: "Qwen2.5 1.5B Instruct (4-bit)",
        family: Family::Qwen25,
        description: "1.5 B parameters, 4-bit. Better wording, about 3× slower than 0.5B.",
        license: APACHE,
        license_url: "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/blob/main/LICENSE",
        author: "Qwen team, Alibaba Cloud",
        source: "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF",
        weights: "qwen2.5-1.5b-instruct-q4_k_m.gguf",
        tokenizer: "tokenizer.json",
        files: &[
            hf!(
                "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
                "91cad51170dc346986eccefdc2dd33a9da36ead9",
                "qwen2.5-1.5b-instruct-q4_k_m.gguf",
                "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e",
                1117320736
            ),
            QWEN_TOKENIZER,
        ],
    },
    ModelInfo {
        id: "smollm2-360m-instruct-q8",
        name: "SmolLM2 360M Instruct (8-bit)",
        family: Family::SmolLm2,
        description: "360 M parameters, 8-bit. The smallest and fastest; simplest wording.",
        license: APACHE,
        license_url: APACHE_URL,
        author: "Hugging Face",
        source: "https://huggingface.co/HuggingFaceTB/SmolLM2-360M-Instruct-GGUF",
        weights: "smollm2-360m-instruct-q8_0.gguf",
        tokenizer: "tokenizer.json",
        files: &[
            hf!(
                "HuggingFaceTB/SmolLM2-360M-Instruct-GGUF",
                "593b5a2e04c8f3e4ee880263f93e0bd2901ad47f",
                "smollm2-360m-instruct-q8_0.gguf",
                "48ab3034d0dd401fbc721eb1df3217902fee7dab9078992d66431f09b7750201",
                386404992
            ),
            SMOL_TOKENIZER,
        ],
    },
    ModelInfo {
        id: "smollm2-1.7b-instruct-q4km",
        name: "SmolLM2 1.7B Instruct (4-bit)",
        family: Family::SmolLm2,
        description: "1.7 B parameters, 4-bit. The largest candidate; slowest.",
        license: APACHE,
        license_url: APACHE_URL,
        author: "Hugging Face",
        source: "https://huggingface.co/HuggingFaceTB/SmolLM2-1.7B-Instruct-GGUF",
        weights: "smollm2-1.7b-instruct-q4_k_m.gguf",
        tokenizer: "tokenizer.json",
        files: &[
            hf!(
                "HuggingFaceTB/SmolLM2-1.7B-Instruct-GGUF",
                "2d4a76a30b4af41ecd395c35725ac11688d4cfe4",
                "smollm2-1.7b-instruct-q4_k_m.gguf",
                "decd2598bc2c8ed08c19adc3c8fdd461ee19ed5708679d1c54ef54a5a30d4f33",
                1055609536
            ),
            SMOL_TOKENIZER,
        ],
    },
    // The fork's fine-tuned explainers (GRADE-PLAN G2.11b, docs/fork/TRAINING.md): LoRA
    // fine-tunes of the bases above, merged and quantized by `llm_quantize`, published as
    // releases `explainer-<name>` of the fork (FORK_RELEASES; the 135M "fast" tune is still a
    // local file: `is_local`, no URL, no download); the checksums pin the published files.
    ModelInfo {
        id: "filmcraft-explainer-qwen05-v1-q8",
        name: "Revolution explainer, Qwen2.5 0.5B tuned (8-bit)",
        family: Family::Qwen25,
        description: "Qwen2.5-0.5B-Instruct (revision 7ae5576) fine-tuned by the fork to explain grade nodes in the checked grammar. 97 % of the held-out cases pass the fact checker.",
        license: APACHE,
        license_url: APACHE_URL,
        author: "Qwen team, Alibaba Cloud (base); Revolution fork (fine-tune)",
        source: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct",
        weights: "qwen05-v1-q8_0.gguf",
        tokenizer: "tokenizer.json",
        files: &[
            fork_release!("explainer-qwen05-v1", "qwen05-v1-q8_0.gguf", "938c99fc943189bb1a4060024f9bb63ad710acd865a8c854ab2e5720da35729a", 525137152),
            QWEN_TOKENIZER,
        ],
    },
    ModelInfo {
        id: "filmcraft-explainer-smol360-v1-q8",
        name: "Revolution explainer, SmolLM2 360M tuned (8-bit)",
        family: Family::SmolLm2,
        description: "SmolLM2-360M-Instruct (revision a10cc15) fine-tuned by the fork to explain grade nodes in the checked grammar. 94 % of the held-out cases pass the fact checker.",
        license: APACHE,
        license_url: APACHE_URL,
        author: "Hugging Face (base); Revolution fork (fine-tune)",
        source: "https://huggingface.co/HuggingFaceTB/SmolLM2-360M-Instruct",
        weights: "smol360-v1-q8_0.gguf",
        tokenizer: "tokenizer.json",
        files: &[
            fork_release!("explainer-smol360-v1", "smol360-v1-q8_0.gguf", "aa7b76d762774d7ce0d34f825e788dd87cb3bb2fa35174a14e3513c63a5f19f0", 384636128),
            SMOL_TOKENIZER,
        ],
    },
    // fork AI2: trained on node lines, comp-graph lines and branch-node notes as well
    ModelInfo {
        id: "filmcraft-explainer-smol360-v3-q8",
        name: "Revolution explainer, SmolLM2 360M tuned v3 (8-bit)",
        family: Family::SmolLm2,
        description: "SmolLM2-360M-Instruct (revision a10cc15) fine-tuned by the fork to explain grade nodes, compositor nodes and node-tree branches in the checked grammar: 36 / 36, 38 / 40 and 12 / 13 held-out node cases, 32 / 32 comp lines and 20 / 20 branch notes pass the fact checker.",
        license: APACHE,
        license_url: APACHE_URL,
        author: "Hugging Face (base); Revolution fork (fine-tune)",
        source: "https://huggingface.co/HuggingFaceTB/SmolLM2-360M-Instruct",
        weights: "smol360-v3-q8_0.gguf",
        tokenizer: "tokenizer.json",
        files: &[
            fork_release!("explainer-smol360-v3", "smol360-v3-q8_0.gguf", "d3be2df8e19d5b94615e450489cd29c124c1cc91323f01b06ee4bb627256e8cd", 384636128),
            SMOL_TOKENIZER,
        ],
    },
    // fork AI1.4: the "fast" option for slower PCs (about half the 360M tune's time per answer);
    // chosen in Settings, never automatically (not in TUNED_PREFERENCE)
    ModelInfo {
        id: "filmcraft-explainer-smol135-v1-q8",
        name: "Revolution explainer, SmolLM2 135M tuned (8-bit, fast)",
        family: Family::SmolLm2,
        description: "SmolLM2-135M-Instruct (revision 12fd25f) fine-tuned by the fork like the 360M tune, for slower computers: about twice as fast, 34 / 36, 36 / 40 and 13 / 13 held-out cases pass the fact checker (the 360M tune: 34, 37, 13). Local file until published.",
        license: APACHE,
        license_url: APACHE_URL,
        author: "Hugging Face (base); Revolution fork (fine-tune)",
        source: "https://huggingface.co/HuggingFaceTB/SmolLM2-135M-Instruct",
        weights: "smol135-v1-q8_0.gguf",
        tokenizer: "tokenizer.json",
        files: &[local!("smol135-v1-q8_0.gguf", "4362af26f7523f39154127996520ecb8ccbff9d50c2683456ab8f30d56e6baf2", 143042208), SMOL_TOKENIZER],
    },
    ModelInfo {
        id: "smollm3-3b-q4km",
        name: "SmolLM3 3B (4-bit)",
        family: Family::SmolLm3,
        description: "3 B parameters, 4-bit. For the assistant (Ask the coach, AI3): about 2 GB, several times slower than the small models.",
        license: APACHE,
        license_url: APACHE_URL,
        author: "Hugging Face",
        source: "https://huggingface.co/ggml-org/SmolLM3-3B-GGUF",
        weights: "SmolLM3-Q4_K_M.gguf",
        tokenizer: "tokenizer.json",
        files: &[
            hf!(
                "ggml-org/SmolLM3-3B-GGUF",
                "4965cb60b150737b68a0408c36aeefb65078f894",
                "SmolLM3-Q4_K_M.gguf",
                "8334b850b7bd46238c16b0c550df2138f0889bf433809008cc17a8b05761863e",
                1915305312
            ),
            SMOL3_TOKENIZER,
        ],
    },
];

/// The default model: **provisional until the 30-case evaluation** (GRADE-PLAN.md item 11).
pub const DEFAULT_MODEL: &str = "qwen2.5-0.5b-instruct-q4km";

/// The fine-tuned explainers in order of preference (best first). [`automatic`] uses the first one
/// installed. Fork AI2: `smol360-v3` first, the only tune trained on comp lines and branch notes
/// (32 / 32 and 20 / 20 where v1 passes 6 and 2; one case short of v1 / v2 on held-out set 3:
/// provisional, OWNER-QUESTIONS Q61); then AI1's qwen05-v1 (35 / 36) and smol360-v1 (34 / 36).
pub const TUNED_PREFERENCE: &[&str] = &["filmcraft-explainer-smol360-v3-q8", "filmcraft-explainer-qwen05-v1-q8", "filmcraft-explainer-smol360-v1-q8"];

/// The model used when the user hasn't chosen one: the best fine-tuned explainer installed in
/// `models_dir`, else [`DEFAULT_MODEL`]. Only checks file sizes (cheap enough per request).
pub fn automatic(models_dir: Option<&Path>) -> Option<&'static ModelInfo> {
    let tuned = models_dir.and_then(|d| TUNED_PREFERENCE.iter().filter_map(|id| find(id)).find(|m| installed(d, m)));
    tuned.or_else(|| find(DEFAULT_MODEL))
}

pub fn catalogue() -> &'static [ModelInfo] {
    CATALOGUE
}

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    CATALOGUE.iter().find(|m| m.id == id)
}

/// Where model `m` lives under `models_dir`.
pub fn model_dir(models_dir: &Path, m: &ModelInfo) -> PathBuf {
    models_dir.join(m.id)
}

/// All files present with their catalogue sizes (contents were verified when downloaded).
pub fn installed(models_dir: &Path, m: &ModelInfo) -> bool {
    let d = model_dir(models_dir, m);
    m.files.iter().all(|f| std::fs::metadata(d.join(f.name)).is_ok_and(|md| md.len() == f.size))
}

/// Bytes still to download for `m` (files already present are skipped).
pub fn missing_bytes(models_dir: &Path, m: &ModelInfo) -> u64 {
    let d = model_dir(models_dir, m);
    m.files.iter().filter(|f| !std::fs::metadata(d.join(f.name)).is_ok_and(|md| md.len() == f.size)).map(|f| f.size).fold(0u64, u64::saturating_add)
}

/// Re-hash every file of `m` in `dir` (the model's own folder) and compare with the catalogue's
/// SHA-256 and size. Reads ≈ the model's size from disk (seconds), so callers run it once after a
/// download or on first use and cache the result.
pub fn verify(dir: &Path, m: &ModelInfo) -> Result<(), crate::LlmError> {
    use crate::LlmError;
    use sha2::Digest;
    use std::io::Read;
    for f in m.files {
        let path = dir.join(f.name);
        let file = std::fs::File::open(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LlmError::NotInstalled(path.display().to_string()),
            _ => LlmError::Io(format!("{}: {e}", path.display())),
        })?;
        let size = file.metadata()?.len();
        if size != f.size {
            return Err(LlmError::BadModel { file: f.name.to_string(), reason: format!("{size} bytes, expected {}", f.size) });
        }
        let mut reader = file.take(f.size);
        let mut hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = reader.read(&mut buf)?;
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else { break };
            hasher.update(chunk);
        }
        let sha: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if sha != f.sha256 {
            return Err(LlmError::BadModel { file: f.name.to_string(), reason: format!("checksum {sha}, expected {}", f.sha256) });
        }
    }
    Ok(())
}

/// The picture model (fork AI-PLAN AI4 item 2): Qwen3-VL 4B Instruct, run on FilmCraft's own code
/// ([`crate::vl::VisionLanguage`]). Kept out of [`catalogue`]: it isn't a GGUF text model the
/// explainer can select; its text half is converted to GGUF on this computer on first use
/// ([`crate::convert`]). Only the files the runtime reads are pinned (the owner approved the
/// download on 2026-10-15).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct VisionModelInfo {
    /// Folder name under the models directory.
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub author: &'static str,
    pub source: &'static str,
    pub files: &'static [ModelFile],
}

impl VisionModelInfo {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).fold(0u64, u64::saturating_add)
    }

    /// All files present with their pinned sizes (contents were verified when downloaded).
    pub fn installed(&self, models_dir: &Path) -> bool {
        let d = models_dir.join(self.id);
        self.files.iter().all(|f| std::fs::metadata(d.join(f.name)).is_ok_and(|md| md.len() == f.size))
    }

    /// Bytes still to download (files already present with the right size are skipped).
    pub fn missing_bytes(&self, models_dir: &Path) -> u64 {
        let d = models_dir.join(self.id);
        self.files.iter().filter(|f| !std::fs::metadata(d.join(f.name)).is_ok_and(|md| md.len() == f.size)).map(|f| f.size).fold(0u64, u64::saturating_add)
    }
}

pub static VISION_MODEL: VisionModelInfo = VisionModelInfo {
    id: "qwen3-vl-4b-instruct",
    name: "Qwen3-VL 4B Instruct (picture model)",
    description: "Looks at the frame under the playhead when you ask about the picture. About 8.9 GB; \
                  needs about 10 GB of free memory while it looks.",
    license: APACHE,
    license_url: APACHE_URL,
    author: "Qwen team, Alibaba Cloud",
    source: "https://huggingface.co/Qwen/Qwen3-VL-4B-Instruct",
    files: &[
        hf!(
            "Qwen/Qwen3-VL-4B-Instruct",
            "ebb281ec70b05090aa6165b016eac8ec08e71b17",
            "config.json",
            "edac7703329133edfc53e46ac0081835144c99d7eebf28b71c732694d435224d",
            1505
        ),
        hf!(
            "Qwen/Qwen3-VL-4B-Instruct",
            "ebb281ec70b05090aa6165b016eac8ec08e71b17",
            "model.safetensors.index.json",
            "58a7841d7bff2548dd91577d216274a83cf1b500bc6a534b809d6c1b1707cf2b",
            64742
        ),
        hf!(
            "Qwen/Qwen3-VL-4B-Instruct",
            "ebb281ec70b05090aa6165b016eac8ec08e71b17",
            "model-00001-of-00002.safetensors",
            "30a01a0556622645a3cce87b655bbbbbc1f170c196099f1b666c93202c3339a9",
            4967229296
        ),
        hf!(
            "Qwen/Qwen3-VL-4B-Instruct",
            "ebb281ec70b05090aa6165b016eac8ec08e71b17",
            "model-00002-of-00002.safetensors",
            "046296a2a387efb43b0c997d5833c789604d168834f6e0d3064bf7bb13d002a6",
            3908490048
        ),
        hf!(
            "Qwen/Qwen3-VL-4B-Instruct",
            "ebb281ec70b05090aa6165b016eac8ec08e71b17",
            "tokenizer.json",
            "a5d85b6dcc535e6b93115a9ef287e6132fdbf30270da6218194ba742261173c7",
            7032403
        ),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn is_hex(s: &str, n: usize) -> bool {
        s.len() == n && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    #[test]
    fn the_vision_model_is_pinned_and_checksummed() {
        let m = &VISION_MODEL;
        assert_eq!(m.license, "Apache-2.0");
        let mut names = std::collections::HashSet::new();
        for f in m.files {
            assert!(names.insert(f.name), "duplicate {}", f.name);
            assert!(is_hex(f.sha256, 64), "{}", f.name);
            assert!(f.url.starts_with("https://huggingface.co/Qwen/Qwen3-VL-4B-Instruct/resolve/") && f.url.ends_with(f.name), "{}", f.url);
            assert!(f.size > 0);
        }
        for need in ["config.json", "model.safetensors.index.json", "tokenizer.json"] {
            assert!(names.contains(need), "{need}");
        }
        assert!((8_800_000_000..9_000_000_000).contains(&m.size()), "{}", m.size());
        let dir = std::env::temp_dir().join(format!("fc-vision-missing-{}", std::process::id()));
        assert!(!m.installed(&dir));
        assert_eq!(m.missing_bytes(&dir), m.size());
    }

    #[test]
    fn catalogue_is_pinned_checksummed_and_licensed() {
        assert_eq!(catalogue().len(), 9);
        assert!(!find(DEFAULT_MODEL).is_some_and(ModelInfo::is_local), "the default must be downloadable");
        assert!(find(DEFAULT_MODEL).is_some());
        let mut ids = std::collections::HashSet::new();
        for m in catalogue() {
            assert!(ids.insert(m.id), "duplicate id {}", m.id);
            assert_eq!(m.license, "Apache-2.0", "{}", m.id);
            assert!(m.license_url.starts_with("https://"));
            assert!(m.source.starts_with("https://huggingface.co/"));
            // (the AI1.4 "fast" 135M tune is ~145 MB; SmolLM3 3B ~1.93 GB)
            assert!(m.size() > 100_000_000 && m.size() < 2_000_000_000, "{}", m.id);
            let w = m.file(m.weights).expect("weights listed");
            assert!(w.name.ends_with(".gguf"));
            let t = m.file(m.tokenizer).expect("tokenizer listed");
            assert_eq!(t.name, "tokenizer.json");
            for f in m.files {
                assert!(f.size > 0);
                assert!(is_hex(f.sha256, 64), "{}", f.name);
                if f.url.is_empty() {
                    // a local file: only the fork's tuned models, and never the tokenizer
                    assert!(m.is_tuned() && f.name == m.weights, "{}: {}", m.id, f.name);
                    continue;
                }
                // a published fine-tune: a release asset of the fork, tag/name
                if let Some(rest) = f.url.strip_prefix(FORK_RELEASES) {
                    assert!(m.is_tuned() && f.name == m.weights, "{}: {}", m.id, f.url);
                    let tag_name = rest.split_once('/');
                    assert!(tag_name.is_some_and(|(tag, name)| !tag.is_empty() && name == f.name), "release url {}", f.url);
                    continue;
                }
                let hf = f.url.strip_prefix("https://huggingface.co/");
                assert!(hf.is_some(), "neither a Hugging Face nor a fork release url: {}", f.url);
                let Some(rest) = hf else { continue };
                let (_, after) = rest.split_once("/resolve/").expect("resolve url");
                let (rev, name) = after.split_once('/').expect("rev/name");
                assert!(is_hex(rev, 40), "not pinned to a commit: {}", f.url);
                assert_eq!(name, f.name);
                assert!(!f.url.contains("/main/"));
            }
        }
    }

    #[test]
    fn tuned_models_are_published_or_local_and_run_without_repeat_penalty() {
        let tuned: Vec<_> = catalogue().iter().filter(|m| m.is_tuned()).collect();
        assert_eq!(tuned.len(), 4);
        for m in tuned {
            // the automatic choices download from the fork's releases; only the "fast" tune is local
            assert_eq!(m.is_local(), !TUNED_PREFERENCE.contains(&m.id), "{}", m.id);
            assert_eq!(m.recommended_options().repeat_penalty, 1.0);
            assert!(m.description.contains("fine-tuned"));
        }
        for m in catalogue().iter().filter(|m| !m.is_tuned()) {
            assert!(!m.is_local(), "{}", m.id);
            assert_eq!(m.recommended_options(), crate::GenOptions::default());
        }
    }

    /// Placeholder files of the catalogue sizes (sparse: no real disk use).
    fn fake_install(dir: &Path, m: &ModelInfo) {
        let d = model_dir(dir, m);
        std::fs::create_dir_all(&d).unwrap();
        for f in m.files {
            std::fs::File::create(d.join(f.name)).unwrap().set_len(f.size).unwrap();
        }
    }

    #[test]
    fn automatic_prefers_an_installed_tune() {
        let dir = std::env::temp_dir().join(format!("filmcraft-llm-auto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let id = |d: Option<&Path>| automatic(d).map(|m| m.id);
        assert_eq!(id(None), Some(DEFAULT_MODEL));
        assert_eq!(id(Some(&dir)), Some(DEFAULT_MODEL), "nothing installed");
        let tuned: Vec<_> = TUNED_PREFERENCE.iter().filter_map(|t| find(t)).collect();
        assert_eq!(tuned.len(), 3);
        assert!(tuned.iter().all(|m| m.is_tuned()));
        fake_install(&dir, tuned[2]);
        assert_eq!(id(Some(&dir)), Some(tuned[2].id));
        fake_install(&dir, tuned[1]);
        assert_eq!(id(Some(&dir)), Some(tuned[1].id));
        fake_install(&dir, tuned[0]);
        assert_eq!(id(Some(&dir)), Some(tuned[0].id), "the best tune first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn families_share_one_tokenizer() {
        for m in catalogue() {
            let t = m.file(m.tokenizer).expect("tokenizer");
            let expect = match m.family {
                Family::Qwen25 => QWEN_TOKENIZER.sha256,
                Family::SmolLm2 => SMOL_TOKENIZER.sha256,
                Family::SmolLm3 => SMOL3_TOKENIZER.sha256,
            };
            assert_eq!(t.sha256, expect);
        }
    }

    #[test]
    fn installed_checks_sizes() {
        let dir = std::env::temp_dir().join(format!("filmcraft-llm-models-{}", std::process::id()));
        let m = find(DEFAULT_MODEL).unwrap();
        assert!(!installed(&dir, m));
        assert_eq!(missing_bytes(&dir, m), m.size());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_checks_size_and_checksum() {
        let dir = std::env::temp_dir().join(format!("filmcraft-llm-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        static FILES: [ModelFile; 1] = [ModelFile {
            name: "a.bin",
            url: "https://example.invalid/a.bin",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            size: 3,
        }];
        let m = ModelInfo { files: &FILES, ..*find(DEFAULT_MODEL).unwrap() };
        assert!(matches!(verify(&dir, &m), Err(crate::LlmError::NotInstalled(_))));
        std::fs::write(dir.join("a.bin"), b"abcd").unwrap();
        assert!(matches!(verify(&dir, &m), Err(crate::LlmError::BadModel { .. })));
        std::fs::write(dir.join("a.bin"), b"abd").unwrap();
        assert!(matches!(verify(&dir, &m), Err(crate::LlmError::BadModel { .. })));
        std::fs::write(dir.join("a.bin"), b"abc").unwrap();
        assert_eq!(verify(&dir, &m), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
