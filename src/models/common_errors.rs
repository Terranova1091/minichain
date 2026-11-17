use thiserror::Error;

#[derive(Debug, Clone, Eq, Error, PartialEq)]
pub enum CodecError {
    #[error(
        "string of {len} bytes does not fit into a u16 length prefix (max {})",
        u16::MAX
    )]
    StringTooLong { len: usize },
}
