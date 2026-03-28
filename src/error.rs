use solana_program::program_error::ProgramError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlabError {
    AccountTooSmall,
    Misaligned,
    OutOfBounds,
    OutOfSpace,
}

impl From<SlabError> for ProgramError {
    fn from(_value: SlabError) -> Self {
        ProgramError::InvalidAccountData
    }
}
