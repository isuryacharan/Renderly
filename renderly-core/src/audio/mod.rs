//! Audio synthesis (TTS) for Phase 1 voiceover tracks.

pub mod denoise;
mod tts;

pub use tts::{
    is_openai_available, is_piper_available, synthesize_to_wav, TtsError, VoiceoverProvider,
};
