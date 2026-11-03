# Third-party components

Prompt embeds and links the following projects. Their licences apply to those
components; Prompt's own code is MIT (see `LICENSE`).

| Component | Use | Licence |
| --- | --- | --- |
| [Ghostty / libghostty-vt](https://github.com/ghostty-org/ghostty) | VT parsing, screens, scrollback, selection, key encoding | MIT |
| [winit](https://github.com/rust-windowing/winit) | Window and input | Apache-2.0 / MIT |
| [wgpu](https://github.com/gfx-rs/wgpu) | GPU rendering (Vulkan / Metal) | Apache-2.0 / MIT |
| [softbuffer](https://github.com/rust-windowing/softbuffer) | CPU frame presentation fallback | Apache-2.0 / MIT |
| [bytemuck](https://github.com/Lokathor/bytemuck) | GPU instance data | Apache-2.0 / MIT / Zlib |
| [cosmic-text](https://github.com/pop-os/cosmic-text) | Font discovery, shaping, glyph rasterisation | Apache-2.0 / MIT |
| [tiny-skia](https://github.com/RazrFalcon/tiny-skia) | 2D rasterisation | BSD-3-Clause |
| [portable-pty](https://github.com/wez/wezterm/tree/main/pty) | Pseudo-terminal process I/O | MIT |
| [arboard](https://github.com/1Password/arboard) | Clipboard | Apache-2.0 / MIT |
| [unicode-segmentation](https://github.com/unicode-rs/unicode-segmentation) | Grapheme clusters | Apache-2.0 / MIT |
| [unicode-width](https://github.com/unicode-rs/unicode-width) | Display width | Apache-2.0 / MIT |
| [serde](https://serde.rs) / [toml](https://github.com/toml-rs/toml) | Config and history | Apache-2.0 / MIT |
| [dirs](https://github.com/dirs-dev/dirs-rs) | Platform directories | Apache-2.0 / MIT |
| [llama.cpp](https://github.com/ggml-org/llama.cpp) | Local Qwen inference (Linux and macOS 26) | MIT |
| [Qwen2.5-Coder](https://huggingface.co/Qwen/Qwen2.5-Coder-1.5B-Instruct-GGUF) | Downloaded on-device command models (1.5B–14B) | Apache-2.0 |
| [ureq](https://github.com/algesten/ureq) | One-time model download | Apache-2.0 / MIT |
| [sha2](https://github.com/RustCrypto/hashes) | Model checksum verification | Apache-2.0 / MIT |

The Qwen model is not part of the repository or the release archive. Prompt
downloads it from Hugging Face on first use and verifies the pinned SHA-256.
The model's Apache-2.0 licence and the llama.cpp MIT licence apply to those
components.

If a zsh binary is bundled with a release via `scripts/vendor-zsh.sh`, that
binary remains under zsh's own licence (MIT-like, see the zsh distribution).
Prompt's source repository does not contain a zsh binary.

The bundled `vendor/ghostty-vt/` contains Ghostty's C headers and a static
library built from the Ghostty source tree. Ghostty's licence text is included
at `vendor/ghostty-vt/LICENSE`.
