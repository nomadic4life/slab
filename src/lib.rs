pub mod error;
pub mod state;

pub use error::SlabError;
pub use state::{FreeNode, NodeType, SlabHeader, SlabMut};
