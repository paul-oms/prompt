//! On-device command generation.
//!
//! macOS 27 has Apple's Foundation Models framework built into the OS. Linux
//! and older macOS releases run a small Qwen GGUF model through llama.cpp
//! instead. Both paths take the same request and return a single shell
//! command, so the composer does not care which one answered.
//!
//! Prompt offers a few current Qwen 3.5 sizes plus MiniCPM5-2B, all Q4, and
//! picks the ones that fit the machine. The 0.8B model is the lightweight
//! base; larger machines can choose 2B, 4B or 9B for better commands. The
//! selected file lives in the data directory, is downloaded once, and then
//! runs offline. Nothing here sends the user's command or output anywhere.

use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use sha2::{Digest, Sha256};

/// One downloadable command model. Q4_K_M keeps the files small enough to run
/// on a laptop while staying accurate enough for shell commands.
#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    /// Total system RAM needed before this model is offered.
    pub min_ram: u64,
    /// RAM needed for this model to be the recommended default rather than
    /// merely possible.
    pub recommended_ram: u64,
    /// Short description for the picker.
    pub note: &'static str,
}

impl ModelSpec {
    pub fn size_label(&self) -> String {
        format!("{:.1} GB", self.bytes as f64 / 1_000_000_000.0)
    }
}

/// Ordered smallest to largest. The first entry is always offered, even on a
/// machine below its minimum, so there is never an empty picker.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "qwen3.5-0.8b",
        name: "Qwen3.5 0.8B",
        file: "Qwen3.5-0.8B-Q4_K_M.gguf",
        url:
            "https://huggingface.co/unsloth/Qwen3.5-0.8B-GGUF/resolve/main/Qwen3.5-0.8B-Q4_K_M.gguf",
        sha256: "bd258782e35f7f458f8aced1adc053e6e92e89bc735ba3be89d38a06121dc517",
        bytes: 532_517_120,
        min_ram: 2 * GIB,
        recommended_ram: 4 * GIB,
        note: "smallest, fastest",
    },
    ModelSpec {
        id: "minicpm5-2b",
        name: "MiniCPM5 2B",
        file: "MiniCPM5-2B-Q4_K_M.gguf",
        url: "https://huggingface.co/openbmb/MiniCPM5-2B-GGUF/resolve/main/MiniCPM5-2B-Q4_K_M.gguf",
        sha256: "ec2d5801640099e97d8d7e8003ad4d81f336e757811f03a26173dddf386602fd",
        bytes: 1_561_318_368,
        min_ram: 4 * GIB,
        recommended_ram: 6 * GIB,
        note: "Sept 2026, balanced",
    },
    ModelSpec {
        id: "qwen3.5-4b",
        name: "Qwen3.5 4B",
        file: "Qwen3.5-4B-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/main/Qwen3.5-4B-Q4_K_M.gguf",
        sha256: "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
        bytes: 2_740_937_888,
        min_ram: 6 * GIB,
        recommended_ram: 8 * GIB,
        note: "good on 8 GB",
    },
    ModelSpec {
        id: "qwen3.5-9b",
        name: "Qwen3.5 9B",
        file: "Qwen3.5-9B-Q4_K_M.gguf",
        url: "https://huggingface.co/unsloth/Qwen3.5-9B-GGUF/resolve/main/Qwen3.5-9B-Q4_K_M.gguf",
        sha256: "03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8",
        bytes: 5_680_522_464,
        min_ram: 10 * GIB,
        recommended_ram: 14 * GIB,
        note: "best commands",
    },
];

const GIB: u64 = 1_073_741_824;
const SELECTION_FILE: &str = "selected-model";

/// The lightweight model shown on every machine: the safe choice when memory
/// is tight.
pub fn default_spec() -> &'static ModelSpec {
    &MODELS[0]
}

pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|spec| spec.id == id)
}

/// Models worth showing on a machine with `ram_bytes` of memory. The default
/// model is always first; larger models only appear when they fit.
pub fn options_for(ram_bytes: u64) -> Vec<&'static ModelSpec> {
    MODELS
        .iter()
        .enumerate()
        .filter(|(index, spec)| *index == 0 || ram_bytes >= spec.min_ram)
        .map(|(_, spec)| spec)
        .collect()
}

/// The largest model that fits with comfortable headroom. On a machine where
/// only the default fits, this is the default.
pub fn recommended_for(ram_bytes: u64) -> &'static ModelSpec {
    let options = options_for(ram_bytes);
    options
        .iter()
        .rev()
        .find(|spec| ram_bytes >= spec.recommended_ram)
        .copied()
        .unwrap_or_else(|| options.last().copied().unwrap_or_else(default_spec))
}

/// Resolution order: `PROMPT_MODEL`, then `[ai] model` in config.toml, then the
/// choice saved by the picker. `None` means ask on the next Ctrl+G.
pub fn resolve_choice(models_dir: &Path, configured: &str) -> Option<&'static ModelSpec> {
    if let Ok(id) = std::env::var("PROMPT_MODEL") {
        if let Some(spec) = find(id.trim()) {
            return Some(spec);
        }
        eprintln!("prompt: PROMPT_MODEL is not a known model id: {id}");
    }
    let configured = configured.trim();
    if !configured.is_empty() {
        if let Some(spec) = find(configured) {
            return Some(spec);
        }
        eprintln!("prompt: [ai] model is not a known model id: {configured}");
    }
    if let Ok(id) = std::fs::read_to_string(models_dir.join(SELECTION_FILE)) {
        if let Some(spec) = find(id.trim()) {
            return Some(spec);
        }
    }
    // If the current default file is already cached (for example from an
    // interrupted first run), do not ask again.
    if models_dir.join(default_spec().file).is_file() {
        return Some(default_spec());
    }
    None
}

pub fn save_choice(models_dir: &Path, id: &str) -> Result<(), String> {
    std::fs::create_dir_all(models_dir)
        .map_err(|err| format!("could not create model directory: {err}"))?;
    std::fs::write(models_dir.join(SELECTION_FILE), format!("{id}\n"))
        .map_err(|err| format!("could not remember the model choice: {err}"))
}

/// Commands longer than this are truncated before they reach the composer.
pub const MAX_LINES: usize = 30;

// Enough for the system prompt, a long intent and a 30-line command. Keeping
// this tight matters on laptops: the KV cache and compute buffer are the
// difference between a snappy first command and swapping. Fix requests also
// carry terminal output, so they get the larger budget below.
const N_CTX: u32 = 2048;
const N_CTX_FIX: u32 = 8192;
const N_BATCH: u32 = 256;
const MAX_OUTPUT_TOKENS: usize = 1536;
const MAX_INTENT_CHARS: usize = 2000;
/// Fix requests include the tail of the terminal output, capped so the prompt
/// stays inside `N_CTX_FIX` even when the failed command printed a lot.
pub const MAX_FIX_OUTPUT_CHARS: usize = 12_000;

const SYSTEM_PROMPT: &str = "\
You turn a request into one shell command for the given shell and platform. \
Reply with the command ready to run: no markdown, no backticks, and no leading $ or >. \
If a short explanation is needed, add it as a comment of at most 20 words. \
The directory line is the current working directory, so prefer relative paths. \
Prefer the shortest correct command. \
If a script is needed, use at most 30 lines. If the request is already a command, return it unchanged. \
Use python3 to run .py files.
Examples:\n\
list all files including hidden ones => ls -la\n\
ssh into example.com as the deploy user => ssh deploy@example.com\n\
find the biggest files here => du -ah . | sort -rh | head -20\n\
run script.py => python3 script.py";

const FIX_SYSTEM_PROMPT: &str = "\
You fix a shell command that failed. You get the shell, platform, working directory, \
the failed command, its exit code and the end of its terminal output. \
Reply with one replacement shell command ready to run: no markdown, no backticks, and no leading $ or >. \
Keep the user's intent and change only what is needed to make the command succeed. \
Use the output to identify the cause, such as a typo, a missing file, a missing tool or a dependency error. \
If a short explanation is needed, add it as a comment of at most 20 words. \
The directory line is the current working directory, so prefer relative paths. \
Prefer the shortest correct command. If a script is needed, use at most 30 lines. \
Examples:\n\
failed command: gti status / exit 127 / output: command not found: gti => git status\n\
failed command: python3 app.py / exit 1 / output: ModuleNotFoundError: No module named requests => pip install requests && python3 app.py\n\
failed command: cd Documnets / exit 1 / output: no such file or directory => cd Documents";

/// The failed command and enough output to explain why it failed. Only fix
/// requests carry this; ordinary command generation never sees command output.
#[derive(Clone, Debug, Default)]
pub struct FixContext {
    pub command: String,
    pub exit_code: Option<i32>,
    pub output: String,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub id: u64,
    pub intent: String,
    pub shell: String,
    pub cwd: String,
    /// Present when the request is a suggested fix rather than a new command.
    pub fix: Option<FixContext>,
    /// The GGUF model to use for this request. Ignored by Apple Foundation
    /// Models, which are built into the OS.
    pub model: &'static ModelSpec,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Debug)]
pub enum Event {
    /// Human-readable progress while the model downloads or loads.
    Status { id: u64, text: String },
    Done {
        id: u64,
        result: Result<String, String>,
        elapsed: Duration,
        backend: &'static str,
    },
}

/// A handle to the background model worker. The worker starts lazily: it does
/// nothing until the first request, so launching Prompt never blocks on AI.
pub struct Client {
    tx: Sender<Request>,
    rx: Receiver<Event>,
    backend: &'static str,
}

impl Client {
    pub fn spawn(models_dir: PathBuf, notify: Box<dyn Fn() + Send + Sync>) -> Self {
        let (req_tx, req_rx) = mpsc::channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        let backend = selected_backend();
        thread::Builder::new()
            .name("prompt-model".into())
            .spawn(move || worker(models_dir, req_rx, evt_tx, notify))
            .expect("spawning model worker");
        Self {
            tx: req_tx,
            rx: evt_rx,
            backend,
        }
    }

    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// True when the OS model handles commands; there is nothing to choose or
    /// download on these Macs.
    pub fn is_apple_fm(&self) -> bool {
        self.backend == "Apple Foundation Models"
    }

    pub fn request(&self, request: Request) {
        let _ = self.tx.send(request);
    }

    pub fn try_recv(&self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

fn selected_backend() -> &'static str {
    #[cfg(target_os = "macos")]
    if crate::apple_fm::available() {
        return "Apple Foundation Models";
    }
    "Qwen"
}

fn worker(
    models_dir: PathBuf,
    requests: Receiver<Request>,
    events: Sender<Event>,
    notify: Box<dyn Fn() + Send + Sync>,
) {
    let mut engine: Option<QwenEngine> = None;
    for request in requests {
        let started = Instant::now();
        if request.cancel.load(Ordering::Relaxed) {
            continue;
        }

        let backend = selected_backend();
        let result = match backend {
            "Apple Foundation Models" => {
                let prompt = format!(
                    "{}\n\n{}",
                    system_prompt_for(&request),
                    build_prompt(&request)
                );
                crate::apple_fm::generate(&prompt).and_then(|text| sanitize(&text))
            }
            _ => {
                let spec = request.model;
                if engine.is_none() {
                    let path = model_path(&models_dir, spec);
                    if let Err(err) = ensure_model(&path, spec, &request, &events, &notify) {
                        send_done(&events, &notify, request.id, Err(err), started, backend);
                        continue;
                    }
                    if request.cancel.load(Ordering::Relaxed) {
                        continue;
                    }
                    status(
                        &events,
                        &notify,
                        request.id,
                        format!("Loading {}…", spec.name),
                    );
                    match QwenEngine::load(&path) {
                        Ok(loaded) => engine = Some(loaded),
                        Err(err) => {
                            send_done(&events, &notify, request.id, Err(err), started, backend);
                            continue;
                        }
                    }
                }
                engine
                    .as_ref()
                    .expect("engine loaded above")
                    .generate(&request, &events, &notify)
            }
        };

        send_done(&events, &notify, request.id, result, started, backend);
        // A model costs more memory than the terminal itself, especially on a
        // large machine. Keep the terminal light between requests: reloading
        // takes a moment and the UI already shows a loading status.
        engine = None;
    }
}

fn send_done(
    events: &Sender<Event>,
    notify: &(dyn Fn() + Send + Sync),
    id: u64,
    result: Result<String, String>,
    started: Instant,
    backend: &'static str,
) {
    let _ = events.send(Event::Done {
        id,
        result,
        elapsed: started.elapsed(),
        backend,
    });
    notify();
}

fn status(events: &Sender<Event>, notify: &(dyn Fn() + Send + Sync), id: u64, text: String) {
    let _ = events.send(Event::Status { id, text });
    notify();
}

fn model_path(models_dir: &Path, spec: &ModelSpec) -> PathBuf {
    if let Some(path) = std::env::var_os("PROMPT_MODEL_PATH") {
        return PathBuf::from(path);
    }
    models_dir.join(spec.file)
}

fn model_url(spec: &ModelSpec) -> String {
    std::env::var("PROMPT_MODEL_URL").unwrap_or_else(|_| spec.url.to_string())
}

/// Download the model if it is not already cached, then verify its checksum.
fn ensure_model(
    path: &Path,
    spec: &ModelSpec,
    request: &Request,
    events: &Sender<Event>,
    notify: &(dyn Fn() + Send + Sync),
) -> Result<(), String> {
    let id = request.id;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create model directory: {err}"))?;
    }

    let custom = std::env::var_os("PROMPT_MODEL_PATH").is_some();
    if path.is_file() {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if custom {
            if size > 0 {
                return Ok(());
            }
            return Err(format!("the model at {} is empty", path.display()));
        }
        if size == spec.bytes {
            return Ok(());
        }
        let _ = std::fs::remove_file(path);
    }
    if custom {
        return Err(format!(
            "PROMPT_MODEL_PATH points at a missing file: {}",
            path.display()
        ));
    }

    let url = model_url(spec);
    status(
        events,
        notify,
        id,
        format!("Downloading {} ({})…", spec.name, spec.size_label()),
    );

    let part = path.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let mut response = ureq::get(&url)
        .call()
        .map_err(|err| format!("could not download the model: {err}"))?;
    let total = response.body().content_length().unwrap_or(spec.bytes);
    let mut reader = response.body_mut().as_reader();
    let mut file =
        std::fs::File::create(&part).map_err(|err| format!("could not write the model: {err}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut done = 0u64;
    let mut last_status = Instant::now();

    loop {
        if request.cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err("cancelled".into());
        }
        let read = reader
            .read(&mut buf)
            .map_err(|err| format!("model download failed: {err}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buf[..read])
            .map_err(|err| format!("could not write the model: {err}"))?;
        hasher.update(&buf[..read]);
        done += read as u64;
        if last_status.elapsed() >= Duration::from_millis(250) {
            let pct = if total == 0 {
                0
            } else {
                (done.saturating_mul(100) / total).min(100)
            };
            status(
                events,
                notify,
                id,
                format!("Downloading {}… {pct}%", spec.name),
            );
            last_status = Instant::now();
        }
    }

    file.flush()
        .map_err(|err| format!("could not write the model: {err}"))?;
    drop(file);

    if request.cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&part);
        return Err("cancelled".into());
    }

    let digest = hex(&hasher.finalize());
    if digest != spec.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err("the downloaded model failed its checksum".into());
    }
    std::fs::rename(&part, path).map_err(|err| format!("could not store the model: {err}"))?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

struct QwenEngine {
    backend: LlamaBackend,
    model: LlamaModel,
    template: LlamaChatTemplate,
}

impl QwenEngine {
    fn load(path: &Path) -> Result<Self, String> {
        let mut backend = LlamaBackend::init().map_err(|err| format!("llama.cpp init: {err}"))?;
        if std::env::var_os("PROMPT_MODEL_LOGS").is_none() {
            backend.void_logs();
        }
        let mut params = LlamaModelParams::default();
        // On Apple Silicon llama.cpp ships with Metal; offload everything.
        // Linux release builds are CPU-only, which is fine for these Q4 models.
        if cfg!(target_os = "macos") {
            params = params.with_n_gpu_layers(999);
        }
        let model = LlamaModel::load_from_file(&backend, path, &params)
            .map_err(|err| format!("could not load Qwen: {err}"))?;
        let template = model
            .chat_template(None)
            .map_err(|err| format!("Qwen has no chat template: {err}"))?;
        Ok(Self {
            backend,
            model,
            template,
        })
    }

    fn generate(
        &self,
        request: &Request,
        events: &Sender<Event>,
        notify: &(dyn Fn() + Send + Sync),
    ) -> Result<String, String> {
        let system = LlamaChatMessage::new("system".into(), system_prompt_for(request).into())
            .map_err(|err| format!("could not build the prompt: {err}"))?;
        let user = LlamaChatMessage::new("user".into(), build_prompt(request))
            .map_err(|err| format!("could not build the prompt: {err}"))?;
        let prompt = self
            .model
            .apply_chat_template(&self.template, &[system, user], true)
            .map_err(|err| format!("could not build the prompt: {err}"))?;
        let tokens = self.model.vocab().tokenize(prompt.as_bytes(), true, true);
        if tokens.is_empty() {
            return Err("the model prompt was empty".into());
        }

        let threads = thread::available_parallelism()
            .map(|n| n.get().min(8) as i32)
            .unwrap_or(4)
            .max(1);
        let n_ctx = if request.fix.is_some() {
            N_CTX_FIX
        } else {
            N_CTX
        };
        let mut ctx = self
            .model
            .new_context(
                &self.backend,
                LlamaContextParams::default()
                    .with_n_ctx(NonZeroU32::new(n_ctx))
                    .with_n_batch(N_BATCH)
                    .with_n_threads(threads)
                    .with_n_threads_batch(threads),
            )
            .map_err(|err| format!("could not start Qwen: {err}"))?;

        status(events, notify, request.id, "Generating…".into());

        let mut batch = LlamaBatch::new(N_BATCH as usize, 1);
        let mut pos = 0i32;
        for chunk in tokens.chunks(N_BATCH as usize) {
            batch.clear();
            let last = chunk.len().saturating_sub(1);
            for (i, token) in chunk.iter().enumerate() {
                batch
                    .add(*token, pos + i as i32, &[0], i == last)
                    .map_err(|err| format!("could not feed the prompt: {err}"))?;
            }
            ctx.decode(&mut batch)
                .map_err(|err| format!("Qwen could not read the prompt: {err}"))?;
            pos += chunk.len() as i32;
        }

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::penalties(self.model.n_vocab(), 64, 1.15, 0.0, 0.0),
            LlamaSampler::temp(0.2),
            LlamaSampler::top_p(0.95, 1),
            LlamaSampler::dist(0x9e37_79b9),
        ]);

        let mut output = Vec::new();
        for _ in 0..MAX_OUTPUT_TOKENS {
            if request.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let token = sampler.sample(&ctx, -1);
            sampler.accept(token);
            if self.model.vocab().is_eog(token) {
                break;
            }
            output.extend_from_slice(&self.model.vocab().token_to_piece(token, true, None));
            if output.len() > 64 * 1024 {
                break;
            }

            batch.clear();
            batch
                .add(token, pos, &[0], true)
                .map_err(|err| format!("could not continue Qwen: {err}"))?;
            ctx.decode(&mut batch)
                .map_err(|err| format!("Qwen stopped unexpectedly: {err}"))?;
            pos += 1;
        }

        let raw = String::from_utf8_lossy(&output);
        sanitize(&raw)
    }
}

fn system_prompt_for(request: &Request) -> &'static str {
    if request.fix.is_some() {
        FIX_SYSTEM_PROMPT
    } else {
        SYSTEM_PROMPT
    }
}

fn build_prompt(request: &Request) -> String {
    if let Some(fix) = &request.fix {
        let output = if fix.output.trim().is_empty() {
            "(no output)".to_string()
        } else {
            truncate_chars(fix.output.trim_end(), MAX_FIX_OUTPUT_CHARS)
        };
        return format!(
            "shell: {}\nplatform: {}\ndirectory: {}\nfailed command: {}\nexit code: {}\nterminal output:\n---\n{}\n---\nrequest: replace the failed command with one that succeeds",
            request.shell,
            std::env::consts::OS,
            request.cwd,
            truncate_chars(fix.command.trim(), MAX_INTENT_CHARS),
            fix.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".into()),
            output
        );
    }
    let intent = truncate_chars(request.intent.trim(), MAX_INTENT_CHARS);
    format!(
        "shell: {}\nplatform: {}\ndirectory: {}\nrequest: {}",
        request.shell,
        std::env::consts::OS,
        request.cwd,
        intent
    )
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// Models occasionally wrap the answer in a fence or add a chatty prefix.
/// Keep the useful part and enforce the 30-line ceiling.
fn sanitize(raw: &str) -> Result<String, String> {
    let mut text = raw
        .replace("<|im_start|>", "")
        .replace("<|im_end|>", "")
        .replace("<|endoftext|>", "")
        .replace("</s>", "");
    text = text.trim().to_string();

    // Qwen3 can emit a reasoning block even with /no_think. It is never part
    // of the command.
    if let Some(end) = text.find("</think>") {
        text = text[end + "</think>".len()..].trim().to_string();
    }

    if let Some(fence) = text.find("```") {
        let after = &text[fence + 3..];
        let after = after
            .split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or(after);
        if let Some(end) = after.rfind("```") {
            text = after[..end].trim().to_string();
        } else {
            text = after.trim().to_string();
        }
    }

    // "command: ls" / "Shell command: ls" are common tiny-model prefixes.
    for prefix in ["command:", "shell command:", "cmd:"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim_start().to_string();
            break;
        }
    }

    // A leading prompt marker is not part of the command.
    for prefix in ["$ ", "> ", "❯ "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.to_string();
            break;
        }
    }

    let mut lines: Vec<&str> = text.lines().collect();
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.truncate(MAX_LINES);
    let text = lines.join("\n").trim().to_string();

    if text.is_empty() {
        Err("the model returned no command".into())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_plain_command() {
        assert_eq!(sanitize("ls -la\n").unwrap(), "ls -la");
    }

    #[test]
    fn sanitize_strips_fences_and_prompt() {
        assert_eq!(
            sanitize("```bash\n$ git status\n```\n").unwrap(),
            "git status"
        );
    }

    #[test]
    fn sanitize_keeps_multiline_scripts() {
        let text = "for f in *; do\n  echo \"$f\"\ndone";
        assert_eq!(sanitize(text).unwrap(), text);
    }

    #[test]
    fn sanitize_caps_at_thirty_lines() {
        let input = (0..40)
            .map(|i| format!("echo {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = sanitize(&input).unwrap();
        assert_eq!(out.lines().count(), MAX_LINES);
        assert!(out.ends_with("echo 29"));
    }

    #[test]
    fn sanitize_rejects_empty() {
        assert!(sanitize("  \n\n").is_err());
    }

    #[test]
    fn sanitize_strips_thinking_block() {
        assert_eq!(
            sanitize(" thinkingthe user wants a list</think>ls -la").unwrap(),
            "ls -la"
        );
    }

    /// Downloads the real model and runs one generation. Ignored by default
    /// because it needs ~1.1 GB and network access.
    #[test]
    #[ignore]
    fn generates_a_command_end_to_end() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/model-e2e");
        let client = Client::spawn(dir, Box::new(|| {}));
        for (id, intent) in [
            (1, "list every file here, including hidden ones"),
            (2, "ssh into example.com as the deploy user"),
            (3, "find the biggest files here"),
            (4, "make a new git branch called feature/login"),
            (5, "extract archive.tar.gz"),
            (6, "show the last 20 lines of app.log"),
            (7, "find files bigger than 100 megabytes"),
            (8, "show disk usage sorted by size"),
            (9, "copy every .conf file in /etc into backup/"),
            (10, "what is using port 3000"),
        ] {
            client.request(Request {
                id,
                intent: intent.into(),
                shell: "zsh".into(),
                cwd: "/tmp".into(),
                fix: None,
                model: default_spec(),
                cancel: Arc::new(AtomicBool::new(false)),
            });
            let deadline = Instant::now() + Duration::from_secs(900);
            loop {
                assert!(Instant::now() < deadline, "generation timed out");
                match client.try_recv() {
                    Some(Event::Status { text, .. }) => eprintln!("status: {text}"),
                    Some(Event::Done {
                        result, elapsed, ..
                    }) => {
                        let command = result.expect("generation failed");
                        eprintln!("{intent}\n  -> ({:.2}s) {command}\n", elapsed.as_secs_f32());
                        assert!(!command.trim().is_empty());
                        assert!(command.lines().count() <= MAX_LINES);
                        break;
                    }
                    None => thread::sleep(Duration::from_millis(100)),
                }
            }
        }
    }

    #[test]
    fn prompt_includes_context() {
        let request = Request {
            id: 1,
            intent: "ssh into the deploy box".into(),
            shell: "zsh".into(),
            cwd: "/srv/app".into(),
            fix: None,
            model: default_spec(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let prompt = build_prompt(&request);
        assert!(prompt.contains("shell: zsh"));
        assert!(prompt.contains("directory: /srv/app"));
        assert!(prompt.contains("ssh into the deploy box"));
    }

    #[test]
    fn fix_prompt_includes_command_exit_and_output() {
        let request = Request {
            id: 1,
            intent: String::new(),
            shell: "bash".into(),
            cwd: "/srv/app".into(),
            fix: Some(FixContext {
                command: "gti status".into(),
                exit_code: Some(127),
                output: "bash: gti: command not found".into(),
            }),
            model: default_spec(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let prompt = build_prompt(&request);
        assert!(system_prompt_for(&request).starts_with("You fix a shell command"));
        assert!(prompt.contains("failed command: gti status"));
        assert!(prompt.contains("exit code: 127"));
        assert!(prompt.contains("bash: gti: command not found"));
    }

    #[test]
    fn model_catalog_is_ordered_and_unique() {
        assert_eq!(default_spec().id, "qwen3.5-0.8b");
        for (index, spec) in MODELS.iter().enumerate() {
            assert!(spec.bytes > 0);
            assert_eq!(spec.sha256.len(), 64);
            assert!(spec.url.ends_with(spec.file));
            assert!(!MODELS[..index].iter().any(|other| other.id == spec.id));
            assert!(!MODELS[..index].iter().any(|other| other.file == spec.file));
            if index > 0 {
                assert!(spec.bytes > MODELS[index - 1].bytes);
                assert!(spec.min_ram >= MODELS[index - 1].min_ram);
            }
        }
    }

    #[test]
    fn small_machines_only_get_the_default() {
        let options = options_for(2 * GIB);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].id, default_spec().id);
        assert_eq!(recommended_for(2 * GIB).id, default_spec().id);
    }

    #[test]
    fn options_grow_with_memory() {
        assert_eq!(options_for(8 * GIB).len(), 3);
        assert_eq!(options_for(12 * GIB).len(), 4);
        assert_eq!(options_for(16 * GIB).len(), 4);
        assert_eq!(options_for(64 * GIB).len(), MODELS.len());
    }

    #[test]
    fn recommendation_leaves_headroom() {
        assert_eq!(recommended_for(8 * GIB).id, "qwen3.5-4b");
        assert_eq!(recommended_for(12 * GIB).id, "qwen3.5-4b");
        assert_eq!(recommended_for(16 * GIB).id, "qwen3.5-9b");
        assert_eq!(recommended_for(32 * GIB).id, "qwen3.5-9b");
        // A model that only just fits is offered but not recommended.
        assert!(options_for(12 * GIB)
            .iter()
            .any(|spec| spec.id == "qwen3.5-9b"));
    }
}
