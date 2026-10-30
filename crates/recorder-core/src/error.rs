#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ffmpeg not usable: {0}")]
    Ffmpeg(String),
    #[error("invalid config: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, Error>;
