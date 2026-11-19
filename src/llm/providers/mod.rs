//! LLM Provider implementations

pub mod chat_compat;
pub mod zarnkina;

pub use chat_compat::ChatCompatProvider;
pub use zarnkina::ZarnkinaProvider;
